//! M11 — the head planner endpoint: one ask, one fragment.
//!
//! Everything below M11 is transport. This is the first milestone that needs
//! fleet-wide truth, because only the head sees every snapshot merged: a node's
//! local plan cannot know that c4 has 12 GiB free *and* that c4's link to t440
//! is 0.26 ms.
//!
//! # What this module is, and what it is not
//!
//! The planner itself is Python and lives in `fleet/` (`fleet/api.py` F4 owns
//! `POST /v1/plan`, `fleet/planner.py` F5 owns the swappable `FleetPlanner`).
//! This module is **the contract for calling it** plus a home for the
//! `FleetPlanner` trait on the Rust side. It is not a second planner and not a
//! second scheduler: [`head_plan`] is the reference decision order so the
//! contract is testable on a box with no head reachable, and [`ask_head`] is
//! the client that talks to the real one.
//!
//! # The shape of the answer
//!
//! A **fragment**, never a broadcast. One node asks, one node gets an answer
//! naming one host, one slot, one model, and the reason. The head's full plan
//! never travels back: [`PlanFragment`] has no field that could carry it, and
//! `fragment_does_not_leak_the_rest_of_the_fleet` asserts that.
//!
//! An unsatisfiable role is a typed `403 escalate-nowhere`, never an empty
//! fragment that looks like success. That is the same rule the fabric applies
//! everywhere else: nothing backing it means no fabricated success.
//!
//! # Reject tokens
//!
//! [`PlanReject::reason_token`] follows the rRCP §4 rule — a machine-stable
//! kebab-case token first, human text after a colon — and reuses §4 tokens
//! where they apply (`bad-enum`, `missing-field`, `backend-down`,
//! `executor-error`). `escalate-nowhere` is deliberately **not** one of the §4
//! job tokens: it is not a job result, it is the head saying "no host in the
//! fleet can take this role". Inventing a §4 token for it would make a plan
//! refusal indistinguishable from a failed job.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Path the head serves the plan endpoint on (`fleet/api.py`, F4).
pub const PLAN_ENDPOINT_PATH: &str = "/v1/plan";

/// The six fields `fleet.api.PlanFragment` declares, in declaration order.
/// A Rust fragment that serializes to exactly this **set** of keys is
/// wire-compatible with the Python response model (JSON object key order is
/// not semantic, and `serde_json` sorts keys without `preserve_order`); the
/// two optional extensions below are `skip_serializing_if = "Option::is_none"`
/// so they vanish when unset.
pub const FRAGMENT_FIELDS: [&str; 6] =
    ["node_id", "role", "host_key", "slot", "model_id", "reason"];

/// Planner kinds, mirroring `fleet.planner.PLANNER_KINDS`.
pub const PLANNER_KINDS: [&str; 2] = ["monolithic", "federated"];

/// What a node sends the head. Field names match `fleet.api.PlanRequest`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlanRequest {
    /// The asking node's `host_key`. The head may not hand a node its own work
    /// back through the remote path.
    pub node_id: String,
    /// A role from the head's fallback chain (`fleet.policy.FALLBACK_CHAIN`).
    pub role: String,
    /// The node's own view, for the head's local-first step. Free-form on
    /// purpose: the head decides what it reads.
    ///
    /// A null snapshot is **omitted**, not sent as `null`: `fleet.api.PlanRequest`
    /// types this `dict = Field(default_factory=dict)`, so pydantic applies the
    /// default only when the key is absent and rejects an explicit `null` with
    /// HTTP 422 (`Input should be a valid dictionary`). Found by asking the
    /// real head, not by a unit test.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub local_snapshot: serde_json::Value,
}

impl PlanRequest {
    pub fn new(node_id: impl Into<String>, role: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            role: role.into(),
            local_snapshot: serde_json::Value::Null,
        }
    }
}

/// One answer for one ask. Mirrors `fleet.api.PlanFragment` /
/// `fleet.planner.PlanFragmentSpec`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PlanFragment {
    pub node_id: String,
    pub role: String,
    /// The host that can take the role. Never the asking node on the remote
    /// path; may be the asking node when its own local plan satisfies it.
    pub host_key: String,
    pub slot: String,
    pub model_id: String,
    pub reason: String,
    /// Extension (M11 agent check: "the lease it should request"). The fabric
    /// resource id to lease on `host_key`, e.g. `gpu.0000:0d:00.0` or
    /// `local.store.fabric`. Absent when the snapshot did not name one — it is
    /// never synthesized from the slot name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<String>,
    /// Extension: measured RPC median RTT (ms) from the asking node to
    /// `host_key`, from the `nauti fleet` latency matrix. `None` means **not
    /// measured** — never 0.0, and never a guess.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_ms: Option<f64>,
}

/// Typed refusal, in the rRCP §4 style. Each variant carries the HTTP status
/// the head uses for it, or `None` when the failure is client-side and no
/// request was ever answered.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PlanReject {
    /// Role is not in the head's fallback chain → HTTP 404.
    #[error("bad-enum: unknown role {role:?}")]
    UnknownRole { role: String },
    /// No host in the fleet can take this role → HTTP 403. The constraint is
    /// what F3's `escalate()` recorded, when it recorded one.
    #[error("escalate-nowhere: {}", constraint.as_deref().unwrap_or("no constraint recorded"))]
    EscalateNowhere { constraint: Option<String> },
    /// A required field was absent or empty → HTTP 422 at the head.
    #[error("missing-field: {field}")]
    MissingField { field: &'static str },
    /// The head could not be reached at all. No status: nothing answered.
    #[error("backend-down: head unreachable: {detail}")]
    HeadUnreachable { detail: String },
    /// The head answered with something that is not a fragment, or refused
    /// with a status this client did not expect.
    #[error("{token}: HTTP {status}: {detail}")]
    BadResponse { status: u16, token: &'static str, detail: String },
    /// Planner kind is not in [`PLANNER_KINDS`]. Never a silent default.
    #[error("bad-enum: unknown planner kind {kind:?}; expected one of monolithic, federated")]
    UnknownPlanner { kind: String },
    /// A `--head` URL this client refuses to dial (non-`http://`, no host).
    #[error("bad-enum: unusable head url: {detail}")]
    BadHeadUrl { detail: String },
}

impl PlanReject {
    /// Machine-stable kebab-case token, per rRCP §4.
    pub fn reason_token(&self) -> &'static str {
        match self {
            PlanReject::UnknownRole { .. } => "bad-enum",
            PlanReject::EscalateNowhere { .. } => "escalate-nowhere",
            PlanReject::MissingField { .. } => "missing-field",
            PlanReject::HeadUnreachable { .. } => "backend-down",
            PlanReject::BadResponse { token, .. } => token,
            PlanReject::UnknownPlanner { .. } => "bad-enum",
            PlanReject::BadHeadUrl { .. } => "bad-enum",
        }
    }

    /// HTTP status the head uses for this refusal, or `None` when the failure
    /// happened client-side (no request was answered).
    pub fn http_status(&self) -> Option<u16> {
        match self {
            PlanReject::UnknownRole { .. } => Some(404),
            PlanReject::EscalateNowhere { .. } => Some(403),
            PlanReject::MissingField { .. } => Some(422),
            PlanReject::BadResponse { status, .. } => Some(*status),
            _ => None,
        }
    }
}

fn default_true() -> bool {
    true
}

/// One serving slot on one host. Mirrors the fields `fleet.planner.FleetPlanner`
/// reads (`slot`, `model_id`, `roles`, `healthy`, `free_mb`).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SlotView {
    #[serde(default)]
    pub slot: String,
    #[serde(default)]
    pub model_id: String,
    /// Roles this slot declares. Empty means "any role it can actually hold",
    /// which is `fleet.planner.FleetPlanner._serves`'s rule, not a wildcard we
    /// invented: an empty `roles` with no `model_id` serves nothing.
    #[serde(default)]
    pub roles: Vec<String>,
    /// An unhealthy slot is never offered, whatever else it has.
    #[serde(default = "default_true")]
    pub healthy: bool,
    #[serde(default)]
    pub free_mb: f64,
    /// Fabric resource id to lease for this slot, when the snapshot names one.
    #[serde(default)]
    pub resource_id: Option<String>,
}

impl SlotView {
    /// Does this slot serve `role`? Same rule as the Python `_serves`, so the
    /// two sides cannot disagree about what "can take it" means.
    pub fn serves(&self, role: &str) -> bool {
        if !self.healthy {
            return false;
        }
        if self.roles.is_empty() {
            return !self.model_id.is_empty();
        }
        self.roles.iter().any(|r| r == role)
    }
}

/// One host's slots. `locked` mirrors `PolicyState.locked_hosts`: a locked host
/// is skipped by `resolve()` and must never be offered as a remote.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct HostView {
    #[serde(default)]
    pub slots: Vec<SlotView>,
    #[serde(default)]
    pub locked: bool,
}

/// The measured latency matrix `nauti fleet` writes
/// (`/var/lib/nauti/fleet-matrix.json`, `kind: nauti_fleet_matrix`).
///
/// `rpc_median_ms[from][to]` is the median fabric RPC round-trip in ms. A JSON
/// `null` — which the live matrix really contains for an unreachable node —
/// stays `None`. It is never read as 0.0 and never filled in: a fabricated RTT
/// would silently reorder placement around a link nobody measured.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct LatencyMatrix {
    #[serde(default)]
    pub rpc_median_ms: BTreeMap<String, BTreeMap<String, Option<f64>>>,
}

impl LatencyMatrix {
    /// Read a `nauti_fleet_matrix` (or `nauti_fleet_view`) file. Unknown keys
    /// in the file are ignored; a file with no matrix yields an empty one,
    /// which scores on headroom alone rather than guessing.
    pub fn load(path: &Path) -> Result<Self, PlanReject> {
        let raw = std::fs::read_to_string(path).map_err(|e| PlanReject::HeadUnreachable {
            detail: format!("cannot read {}: {e}", path.display()),
        })?;
        let value: serde_json::Value =
            serde_json::from_str(&raw).map_err(|e| PlanReject::BadResponse {
                status: 0,
                token: "executor-error",
                detail: format!("{} is not JSON: {e}", path.display()),
            })?;
        Ok(Self::from_json(&value))
    }

    /// Accept the matrix shape directly, or a `nauti_fleet_view` whose per-node
    /// `rpc_rtt_ms.median` is flattened into rows keyed by the view's `from`.
    pub fn from_json(value: &serde_json::Value) -> Self {
        if let Ok(m) = serde_json::from_value::<LatencyMatrix>(value.clone()) {
            if !m.rpc_median_ms.is_empty() {
                return m;
            }
        }
        // nauti_fleet_view: {"from": "t440cesarops", "nodes": [{"node": ...,
        // "rpc_rtt_ms": {"median": 0.5}}]}
        let mut out = Self::default();
        let from = value["from"].as_str().unwrap_or_default().to_string();
        if from.is_empty() {
            return out;
        }
        let mut row = BTreeMap::new();
        for node in value["nodes"].as_array().into_iter().flatten() {
            let to = match node["node"].as_str() {
                Some(t) => t.to_string(),
                None => continue,
            };
            let median = node["rpc_rtt_ms"]["median"].as_f64();
            row.insert(to, median);
        }
        if !row.is_empty() {
            out.rpc_median_ms.insert(from, row);
        }
        out
    }

    /// Measured median RTT, or `None` when that pair was never measured.
    pub fn rtt_ms(&self, from: &str, to: &str) -> Option<f64> {
        self.rpc_median_ms.get(from)?.get(to).copied().flatten()
    }
}

/// FLEET-scope scoring term (M11 bullet 3).
///
/// A **planning weight, not a measurement** — same discipline as the fabric's
/// 12 GB/s copy figure: it only ranks candidates that already satisfy the role,
/// and it can never make an unhealthy, locked, or non-serving host eligible.
/// 512 MiB of headroom per millisecond of measured RTT means a 3 ms link has to
/// offer ~1.5 GiB more free VRAM than a 0.3 ms link to win.
pub const LATENCY_MB_PER_MS: f64 = 512.0;

/// Score a candidate. An unmeasured link (`None`) is scored on headroom alone:
/// the term is simply not applied, and the fragment says so by carrying
/// `rtt_ms: None`.
pub fn fleet_score(free_mb: f64, rtt_ms: Option<f64>) -> f64 {
    match rtt_ms {
        Some(ms) => free_mb - (LATENCY_MB_PER_MS * ms),
        None => free_mb,
    }
}

/// The fleet as the head sees it: every host's slots plus the measured
/// latency matrix. This is the merged view F1 (`merge_snapshots`) produces —
/// the thing a node cannot build for itself.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct FleetView {
    #[serde(default)]
    pub hosts: BTreeMap<String, HostView>,
    #[serde(default)]
    pub latency: LatencyMatrix,
}

impl FleetView {
    /// Normalise a snapshot envelope the way `fleet.planner.FleetPlanner._hosts`
    /// and `._slots` do, so both sides read the same bytes the same way:
    ///
    /// - the host map may sit under `nodes`, `hosts` or `fleet`, or be the
    ///   whole object;
    /// - a host's `slots` may be a list, or a `{slot_name: {...}}` dict, or
    ///   absent (then the host object itself is read as the slot list);
    /// - anything that is not an object is dropped rather than guessed at.
    pub fn from_snapshot_json(value: &serde_json::Value) -> Self {
        let mut view = Self::default();
        let hosts = ["nodes", "hosts", "fleet"]
            .iter()
            .find_map(|k| value[k].as_object())
            .or_else(|| value.as_object());
        let Some(hosts) = hosts else {
            return view;
        };
        for (host_key, host_value) in hosts {
            // Skip the envelope's own scalar keys (e.g. a view's "from").
            let Some(host_info) = host_value.as_object() else {
                continue;
            };
            let mut hv = HostView {
                // `serde_json::Map`'s Index impl PANICS on a missing key, so
                // every lookup here goes through `get`.
                locked: host_info
                    .get("locked")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                slots: Vec::new(),
            };
            // Python: `slots = host_info.get("slots", host_info)` — the fallback
            // is the HOST object, never the envelope.
            let slots_value = match host_info.get("slots") {
                Some(v) => v,
                None => host_value,
            };
            let items: Vec<(String, &serde_json::Value)> = match slots_value {
                serde_json::Value::Array(list) => list
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (i.to_string(), v))
                    .collect(),
                serde_json::Value::Object(map) => {
                    map.iter().map(|(k, v)| (k.clone(), v)).collect()
                }
                _ => Vec::new(),
            };
            for (key, item) in items {
                let Ok(mut slot) = serde_json::from_value::<SlotView>(item.clone()) else {
                    continue;
                };
                if slot.slot.is_empty() {
                    slot.slot = key;
                }
                // "healthy" absent already means healthy: `SlotView::healthy`
                // carries `#[serde(default = "default_true")]`, matching
                // Python's `.get("healthy", True)`.
                hv.slots.push(slot);
            }
            view.hosts.insert(host_key.clone(), hv);
        }
        view.latency = LatencyMatrix::from_json(value);
        view
    }

    /// Read a snapshot file and normalise it.
    pub fn load_snapshot(path: &Path) -> Result<Self, PlanReject> {
        let raw = std::fs::read_to_string(path).map_err(|e| PlanReject::HeadUnreachable {
            detail: format!("cannot read {}: {e}", path.display()),
        })?;
        let value: serde_json::Value =
            serde_json::from_str(&raw).map_err(|e| PlanReject::BadResponse {
                status: 0,
                token: "executor-error",
                detail: format!("{} is not JSON: {e}", path.display()),
            })?;
        Ok(Self::from_snapshot_json(&value))
    }

    /// Attach a latency matrix (e.g. one loaded separately from
    /// `/var/lib/nauti/fleet-matrix.json`).
    pub fn with_latency(mut self, latency: LatencyMatrix) -> Self {
        self.latency = latency;
        self
    }

    /// Best serving slot on one host by raw headroom, or `None`. A locked host
    /// has no best slot: it is not a candidate at all.
    pub fn best_slot(&self, host_key: &str, role: &str) -> Option<&SlotView> {
        let host = self.hosts.get(host_key)?;
        if host.locked {
            return None;
        }
        host.slots
            .iter()
            .filter(|s| s.serves(role))
            .max_by(|a, b| a.free_mb.partial_cmp(&b.free_mb).unwrap_or(std::cmp::Ordering::Equal))
    }
}


/// The roles the head knows. Mirrors `fleet.policy.FALLBACK_CHAIN`: a role not
/// in the catalogue is a typed 404, never a shrug. There is no `Default` impl —
/// an empty catalogue has to be written down on purpose.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoleCatalogue {
    roles: Vec<String>,
}

impl RoleCatalogue {
    pub fn new(roles: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self { roles: roles.into_iter().map(Into::into).collect() }
    }

    /// Read a catalogue from JSON: either `["coder", ...]` or the
    /// `FALLBACK_CHAIN` shape `{"coder": ["coder-lane-a", ...], ...}` (keys).
    /// Keys beginning with `_` are comments — the convention the fleet's own
    /// JSON files use (`_comment`, `_provenance`) — and are never roles.
    pub fn from_json(value: &serde_json::Value) -> Self {
        match value {
            serde_json::Value::Array(list) => Self::new(
                list.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .filter(|r| !r.starts_with('_')),
            ),
            serde_json::Value::Object(map) => {
                Self::new(map.keys().filter(|k| !k.starts_with('_')).cloned())
            }
            _ => Self::new(Vec::<String>::new()),
        }
    }

    pub fn contains(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }

    pub fn roles(&self) -> &[String] {
        &self.roles
    }

    pub fn len(&self) -> usize {
        self.roles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.roles.is_empty()
    }
}

/// F5's `FleetPlanner`, on the Rust side. One interface, two implementations,
/// identical signature, so a fragment from either satisfies the same contract.
///
/// Neither implementation may return an empty or "default" fragment for a role
/// it cannot satisfy: unsatisfied means `None`. That is what stops a plan from
/// looking successful while nothing backs it.
pub trait FleetPlanner: std::fmt::Debug {
    /// `monolithic` or `federated` — the name a scorecard records.
    fn name(&self) -> &'static str;

    /// Return a fragment for `ask`, or `None` when the role cannot be
    /// satisfied at this planner's extent.
    fn plan(&self, ask: &PlanRequest, view: &FleetView) -> Option<PlanFragment>;
}

/// Shared helpers, free functions rather than per-impl methods so the two
/// planners cannot drift apart — the same reason the Python ABC holds them.
fn best_slot_scored<'a>(
    host: &'a HostView,
    role: &str,
    rtt_ms: Option<f64>,
) -> Option<(f64, &'a SlotView)> {
    if host.locked {
        return None;
    }
    host.slots
        .iter()
        .filter(|s| s.serves(role))
        .map(|s| (fleet_score(s.free_mb, rtt_ms), s))
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
}

fn fragment_for(
    ask: &PlanRequest,
    host_key: &str,
    slot: &SlotView,
    rtt_ms: Option<f64>,
    reason: String,
) -> PlanFragment {
    PlanFragment {
        node_id: ask.node_id.clone(),
        role: ask.role.clone(),
        host_key: host_key.to_string(),
        slot: slot.slot.clone(),
        model_id: slot.model_id.clone(),
        reason,
        resource_id: slot.resource_id.clone(),
        rtt_ms,
    }
}

/// Fleet-wide view: any healthy, unlocked host **except the asker** may take
/// the role. The head is the natural home for this one — it is the only place
/// that sees every snapshot merged.
#[derive(Clone, Copy, Debug, Default)]
pub struct MonolithicMoEPlanner;

impl FleetPlanner for MonolithicMoEPlanner {
    fn name(&self) -> &'static str {
        "monolithic"
    }

    fn plan(&self, ask: &PlanRequest, view: &FleetView) -> Option<PlanFragment> {
        let mut best: Option<(f64, String, &SlotView, Option<f64>)> = None;
        for (host_key, host) in &view.hosts {
            // The head may not hand a node its own work back.
            if host_key == &ask.node_id {
                continue;
            }
            let rtt = view.latency.rtt_ms(&ask.node_id, host_key);
            let Some((score, slot)) = best_slot_scored(host, &ask.role, rtt) else {
                continue;
            };
            let better = match &best {
                None => true,
                Some((prev, ..)) => score > *prev,
            };
            if better {
                best = Some((score, host_key.clone(), slot, rtt));
            }
        }
        let (_, host_key, slot, rtt) = best?;
        let latency = match rtt {
            Some(ms) => format!("rtt {ms:.3} ms"),
            None => "rtt not measured".to_string(),
        };
        Some(fragment_for(
            ask,
            &host_key,
            slot,
            rtt,
            format!(
                "fleet-wide: {host_key} has a healthy {} slot ({} MiB free, {latency})",
                ask.role, slot.free_mb
            ),
        ))
    }
}

/// Per-node view: only the asking node's own slots are consulted. Needs no
/// head at all — which is the point of keeping it behind the same trait.
#[derive(Clone, Copy, Debug, Default)]
pub struct FederatedPerNodePlanner;

impl FleetPlanner for FederatedPerNodePlanner {
    fn name(&self) -> &'static str {
        "federated"
    }

    fn plan(&self, ask: &PlanRequest, view: &FleetView) -> Option<PlanFragment> {
        let host = view.hosts.get(&ask.node_id)?;
        // Latency is not a placement term for a slot on the asking node itself.
        let (_, slot) = best_slot_scored(host, &ask.role, None)?;
        Some(fragment_for(
            ask,
            &ask.node_id,
            slot,
            None,
            format!("local: {} holds a healthy {} slot", ask.node_id, ask.role),
        ))
    }
}

/// Factory. An unknown kind is a typed error — never a silent default.
pub fn get_planner(kind: &str) -> Result<Box<dyn FleetPlanner>, PlanReject> {
    match kind {
        "monolithic" => Ok(Box::new(MonolithicMoEPlanner)),
        "federated" => Ok(Box::new(FederatedPerNodePlanner)),
        other => Err(PlanReject::UnknownPlanner { kind: other.to_string() }),
    }
}


/// What F3's `escalate()` would record, derived from what this view actually
/// shows. Returns `None` when there is nothing observed to name — an empty
/// fleet is not a constraint claim, it is an absence of evidence, and the
/// header the head sends (`X-Escalate-Constraint`) is omitted in that case
/// exactly as `fleet/api.py` omits it.
fn escalate_constraint(ask: &PlanRequest, view: &FleetView) -> Option<String> {
    if view.hosts.is_empty() {
        return None;
    }
    let locked: Vec<&String> = view
        .hosts
        .iter()
        .filter(|(_, h)| h.locked)
        .map(|(k, _)| k)
        .collect();
    let unhealthy = view
        .hosts
        .values()
        .flat_map(|h| h.slots.iter())
        .filter(|s| !s.healthy && s.roles.iter().any(|r| r == &ask.role))
        .count();
    let mut detail = format!(
        "no healthy unlocked slot serves role {:?} across {} host(s)",
        ask.role,
        view.hosts.len()
    );
    if !locked.is_empty() {
        let mut names: Vec<&str> = locked.iter().map(|s| s.as_str()).collect();
        names.sort_unstable();
        detail.push_str(&format!("; locked: {}", names.join(", ")));
    }
    if unhealthy > 0 {
        detail.push_str(&format!("; {unhealthy} serving slot(s) unhealthy"));
    }
    Some(detail)
}

/// The head's validation step, factored out so every entry point rejects the
/// same asks in the same way: missing field first, then unknown role.
pub fn validate_ask(req: &PlanRequest, roles: &RoleCatalogue) -> Result<(), PlanReject> {
    if req.node_id.trim().is_empty() {
        return Err(PlanReject::MissingField { field: "node_id" });
    }
    if req.role.trim().is_empty() {
        return Err(PlanReject::MissingField { field: "role" });
    }
    if !roles.contains(&req.role) {
        return Err(PlanReject::UnknownRole { role: req.role.clone() });
    }
    Ok(())
}

/// Validate the ask, then let `planner` answer it at its own extent.
///
/// `None` from the planner becomes a typed `403 escalate-nowhere`, with the
/// planner's name prefixed onto the constraint so a reader can tell which
/// extent was consulted (`federated` looks at one node; `monolithic` at the
/// whole fleet). A planner that cannot satisfy the role never yields an empty
/// fragment here.
pub fn plan_with(
    planner: &dyn FleetPlanner,
    req: &PlanRequest,
    view: &FleetView,
    roles: &RoleCatalogue,
) -> Result<PlanFragment, PlanReject> {
    validate_ask(req, roles)?;
    planner.plan(req, view).ok_or_else(|| PlanReject::EscalateNowhere {
        constraint: escalate_constraint(req, view)
            .map(|detail| format!("{}: {detail}", planner.name())),
    })
}

/// Which decision order a planner kind implies.
///
/// `monolithic` is the head's order: local plan first, then fleet-wide
/// ([`head_plan`]). `federated` consults only the asking node, so it has no
/// remote step at all — that is the whole point of it needing no head. An
/// unknown kind is a typed error, never a silent default.
///
/// This is the entry point `nauti plan offline` calls, so the CLI cannot drift
/// from the head's order by picking a planner and forgetting local-first.
pub fn plan_by_kind(
    kind: &str,
    req: &PlanRequest,
    view: &FleetView,
    roles: &RoleCatalogue,
) -> Result<PlanFragment, PlanReject> {
    validate_ask(req, roles)?;
    match kind {
        "monolithic" => head_plan(req, view, roles),
        "federated" => plan_with(&FederatedPerNodePlanner, req, view, roles),
        other => Err(PlanReject::UnknownPlanner { kind: other.to_string() }),
    }
}

/// The head's decision order, as `fleet/api.py` F4 implements it:
///
/// 1. **local first** — the node owns this decision, not the head. A healthy
///    unlocked slot on the asking node answers the ask.
/// 2. **remote** — nothing local, so ask for a different host. The asker is
///    excluded and locked hosts are skipped.
/// 3. **`403 escalate-nowhere`** — unsatisfiable fleet-wide. Said out loud,
///    never an empty fragment that looks like success.
///
/// `roles` is the head's fallback-chain catalogue; a role outside it is a
/// typed 404 before anything is planned.
pub fn head_plan(
    req: &PlanRequest,
    view: &FleetView,
    roles: &RoleCatalogue,
) -> Result<PlanFragment, PlanReject> {
    validate_ask(req, roles)?;

    // 1. Local plan first.
    if let Some(slot) = view.best_slot(&req.node_id, &req.role) {
        return Ok(fragment_for(
            req,
            &req.node_id,
            slot,
            None,
            "local plan satisfies this role".to_string(),
        ));
    }

    // 2. Nothing local: ask for a remote host that can take it.
    if let Some(fragment) = MonolithicMoEPlanner.plan(req, view) {
        return Ok(fragment);
    }

    // 3. Unsatisfiable fleet-wide.
    Err(PlanReject::EscalateNowhere { constraint: escalate_constraint(req, view) })
}


/// Default port when a `--head` URL names none. The policy head has been
/// served on 8080 on the fleet boxes.
pub const DEFAULT_HEAD_PORT: u16 = 8080;

/// Split an `http://host[:port]` authority. Anything else is refused with a
/// typed error rather than dialed: this client speaks plain HTTP/1.1 and does
/// not implement TLS, so an `https://` head must be reached another way.
fn parse_head_url(base: &str) -> Result<(String, u16), PlanReject> {
    let bad = |detail: String| PlanReject::BadHeadUrl { detail };
    let rest = base
        .strip_prefix("http://")
        .ok_or_else(|| bad(format!("{base:?} is not an http:// authority (no TLS here)")))?;
    let authority = rest.split('/').next().unwrap_or("").trim();
    if authority.is_empty() {
        return Err(bad(format!("{base:?} has no host")));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h,
            p.parse::<u16>()
                .map_err(|_| bad(format!("{base:?} has a non-numeric port {p:?}")))?,
        ),
        None => (authority, DEFAULT_HEAD_PORT),
    };
    if host.trim_matches(|c| c == '[' || c == ']').is_empty() {
        return Err(bad(format!("{base:?} has an empty host")));
    }
    Ok((host.to_string(), port))
}

/// Ask the head for a plan fragment. One ask, one answer.
///
/// Maps the head's statuses onto typed refusals: `403` →
/// [`PlanReject::EscalateNowhere`] (constraint taken from the
/// `X-Escalate-Constraint` header, else the JSON `detail`), `404` →
/// [`PlanReject::UnknownRole`], `5xx` → `backend-down`, anything else →
/// `executor-error`. A 200 whose body is not a fragment is `missing-field`,
/// because that is what it is: the contract was not met.
pub fn ask_head(base_url: &str, req: &PlanRequest) -> Result<PlanFragment, PlanReject> {
    ask_head_with_timeout(base_url, req, Duration::from_secs(10))
}

/// [`ask_head`] with an explicit socket timeout.
pub fn ask_head_with_timeout(
    base_url: &str,
    req: &PlanRequest,
    timeout: Duration,
) -> Result<PlanFragment, PlanReject> {
    let (host, port) = parse_head_url(base_url)?;
    let body = serde_json::to_string(req).map_err(|e| PlanReject::BadResponse {
        status: 0,
        token: "executor-error",
        detail: format!("cannot encode PlanRequest: {e}"),
    })?;
    let (status, headers, resp) = http_post(&host, port, PLAN_ENDPOINT_PATH, &body, timeout)?;
    match status {
        200..=299 => {
            serde_json::from_str::<PlanFragment>(&resp).map_err(|e| PlanReject::BadResponse {
                status,
                token: "missing-field",
                detail: format!("body is not a PlanFragment: {e}"),
            })
        }
        403 => Err(PlanReject::EscalateNowhere {
            constraint: headers
                .get("x-escalate-constraint")
                .filter(|s| !s.trim().is_empty())
                .cloned()
                .or_else(|| json_detail(&resp)),
        }),
        404 => Err(PlanReject::UnknownRole { role: req.role.clone() }),
        other => Err(PlanReject::BadResponse {
            status: other,
            token: if other >= 500 { "backend-down" } else { "executor-error" },
            detail: truncate(&resp, 200),
        }),
    }
}

/// FastAPI renders `HTTPException(403, "escalate-nowhere")` as
/// `{"detail": "escalate-nowhere"}`. Read that when the header is absent.
fn json_detail(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value["detail"].as_str().map(str::to_string)
}

fn truncate(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(max).collect();
    format!("{cut}…")
}


/// Minimal HTTP/1.1 POST over a plain socket.
///
/// Deliberately hand-rolled and dependency-free: `nauti-fabric` ships no HTTP
/// client today, and adding `reqwest`/`hyper` for one endpoint would pull a
/// TLS stack into a crate whose default build is dependency-light. This speaks
/// exactly one request with `Connection: close` and reads the answer to EOF.
fn http_post(
    host: &str,
    port: u16,
    path: &str,
    body: &str,
    timeout: Duration,
) -> Result<(u16, BTreeMap<String, String>, String), PlanReject> {
    let authority = format!("{host}:{port}");
    let unreachable = |detail: String| PlanReject::HeadUnreachable { detail };
    let mut stream = std::net::TcpStream::connect(&authority)
        .map_err(|e| unreachable(format!("connect {authority}: {e}")))?;
    // A timeout on both halves: a head that accepts and never answers is the
    // failure mode that must not hang a node's escalation path.
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {authority}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Accept: application/json\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| unreachable(format!("write to {authority}: {e}")))?;
    stream
        .flush()
        .map_err(|e| unreachable(format!("flush to {authority}: {e}")))?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| unreachable(format!("read from {authority}: {e}")))?;
    parse_http_response(&raw).ok_or_else(|| {
        unreachable(format!("{authority} closed with no complete HTTP response"))
    })
}

/// Split a response into status, lower-cased headers and body. Handles both
/// `Content-Length` (read to EOF) and `Transfer-Encoding: chunked`.
fn parse_http_response(raw: &[u8]) -> Option<(u16, BTreeMap<String, String>, String)> {
    let split = find_header_end(raw)?;
    let head = std::str::from_utf8(&raw[..split]).ok()?;
    let body_bytes = &raw[split..];
    let mut lines = head.lines();
    let status_line = lines.next()?;
    let status = status_line
        .split_whitespace()
        .nth(1)?
        .parse::<u16>()
        .ok()?;
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let body = if headers
        .get("transfer-encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
    {
        dechunk(body_bytes)
    } else {
        String::from_utf8_lossy(body_bytes).into_owned()
    };
    Some((status, headers, body))
}

/// Byte offset just past the `\r\n\r\n` (or bare `\n\n`) header terminator.
fn find_header_end(raw: &[u8]) -> Option<usize> {
    if let Some(i) = find_subslice(raw, b"\r\n\r\n") {
        return Some(i + 4);
    }
    find_subslice(raw, b"\n\n").map(|i| i + 2)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Decode a chunked body. A malformed chunk ends the decode rather than
/// guessing: a truncated fragment is worse than an absent one.
fn dechunk(body: &[u8]) -> String {
    let mut out: Vec<u8> = Vec::new();
    let mut rest = body;
    loop {
        let line_end = find_subslice(rest, b"\r\n").unwrap_or(rest.len());
        let size_hex = String::from_utf8_lossy(&rest[..line_end]);
        let size_hex = size_hex.split(';').next().unwrap_or("").trim();
        let Ok(len) = usize::from_str_radix(size_hex, 16) else {
            break;
        };
        if len == 0 {
            break;
        }
        let start = line_end + 2;
        let Some(end) = start.checked_add(len).filter(|e| *e <= rest.len()) else {
            break;
        };
        out.extend_from_slice(&rest[start..end]);
        rest = &rest[end.min(rest.len()).saturating_add(2).min(rest.len())..];
    }
    String::from_utf8_lossy(&out).into_owned()
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn slot(name: &str, model: &str, roles: &[&str], healthy: bool, free_mb: f64) -> SlotView {
        SlotView {
            slot: name.into(),
            model_id: model.into(),
            roles: roles.iter().map(|s| s.to_string()).collect(),
            healthy,
            free_mb,
            resource_id: None,
        }
    }

    fn host(slots: Vec<SlotView>) -> HostView {
        HostView { slots, locked: false }
    }

    fn view_with(hosts: Vec<(&str, HostView)>) -> FleetView {
        FleetView {
            hosts: hosts.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            latency: LatencyMatrix::default(),
        }
    }

    /// The head's fallback-chain catalogue, shaped like `FALLBACK_CHAIN`.
    fn catalogue() -> RoleCatalogue {
        RoleCatalogue::new(["coder", "thinker", "reviewer"])
    }

    /// The field-name set a serialized value carries. Sorted, because JSON
    /// object key order is not semantic and `serde_json` sorts without
    /// `preserve_order`.
    fn wire_keys(value: &serde_json::Value) -> Vec<String> {
        let mut keys: Vec<String> =
            value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    }

    fn expected_fragment_keys() -> Vec<String> {
        let mut keys = FRAGMENT_FIELDS.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        keys.sort();
        keys
    }

    // ── the wire contract must match fleet/api.py ────────────────────────────

    #[test]
    fn fragment_serializes_to_exactly_the_python_field_names() {
        let f = PlanFragment {
            node_id: "t440cesarops".into(),
            role: "coder".into(),
            host_key: "cesarops4om".into(),
            slot: "coder-lane-c4".into(),
            model_id: "KAT-Coder-V2.5".into(),
            reason: "fleet-wide".into(),
            resource_id: None,
            rtt_ms: None,
        };
        let value = serde_json::to_value(&f).unwrap();
        assert_eq!(wire_keys(&value), expected_fragment_keys());
    }

    #[test]
    fn python_shaped_fragment_deserializes_without_the_extensions() {
        // Exactly what fleet/api.py's PlanFragment response model emits.
        let raw = r#"{"node_id":"t440cesarops","role":"coder","host_key":"cesarops4om",
                      "slot":"coder-lane-c4","model_id":"KAT-Coder-V2.5",
                      "reason":"no local slot for coder; remote host cesarops4om can take it"}"#;
        let f: PlanFragment = serde_json::from_str(raw).unwrap();
        assert_eq!(f.host_key, "cesarops4om");
        assert_eq!(f.resource_id, None);
        assert_eq!(f.rtt_ms, None);
    }

    #[test]
    fn a_null_snapshot_is_omitted_not_sent_as_null() {
        // Regression for what the REAL head rejected with HTTP 422:
        // `local_snapshot: dict = Field(default_factory=dict)` accepts an
        // absent key but not an explicit null.
        let value = serde_json::to_value(PlanRequest::new("t440cesarops", "coder")).unwrap();
        assert_eq!(value["node_id"], "t440cesarops");
        assert_eq!(value["role"], "coder");
        assert!(
            value.get("local_snapshot").is_none(),
            "null local_snapshot must be omitted, got {value}"
        );
        let wire = serde_json::to_string(&PlanRequest::new("t440cesarops", "coder")).unwrap();
        assert!(!wire.contains("null"), "null must never reach the wire: {wire}");

        // A snapshot the node did build is still sent.
        let mut req = PlanRequest::new("t440cesarops", "coder");
        req.local_snapshot = serde_json::json!({"slots": [{"slot": "coder-lane-a"}]});
        let value = serde_json::to_value(&req).unwrap();
        assert!(value["local_snapshot"].is_object(), "{value}");
        // Round-trips back to the same request.
        assert_eq!(serde_json::from_value::<PlanRequest>(value).unwrap(), req);
    }


    // ── the decision order (M11 agent check) ─────────────────────────────────

    #[test]
    fn unknown_role_is_a_typed_404_not_a_shrug() {
        let view = view_with(vec![(
            "cesarops4om",
            host(vec![slot("coder-lane-c4", "KAT", &["coder"], true, 8000.0)]),
        )]);
        let err = head_plan(&PlanRequest::new("t440cesarops", "bard"), &view, &catalogue())
            .expect_err("an unknown role must not plan");
        assert_eq!(err, PlanReject::UnknownRole { role: "bard".into() });
        assert_eq!(err.reason_token(), "bad-enum");
        assert_eq!(err.http_status(), Some(404));
    }

    #[test]
    fn empty_node_or_role_is_missing_field() {
        let view = view_with(vec![]);
        let err = head_plan(&PlanRequest::new("", "coder"), &view, &catalogue()).unwrap_err();
        assert_eq!(err, PlanReject::MissingField { field: "node_id" });
        assert_eq!(err.http_status(), Some(422));
        let err =
            head_plan(&PlanRequest::new("t440cesarops", " "), &view, &catalogue()).unwrap_err();
        assert_eq!(err, PlanReject::MissingField { field: "role" });
    }

    #[test]
    fn local_plan_is_preferred_and_names_the_asking_node() {
        let view = view_with(vec![
            (
                "t440cesarops",
                host(vec![slot("coder-lane-a", "Ornith-35B", &["coder"], true, 4000.0)]),
            ),
            (
                "cesarops4om",
                host(vec![slot("coder-lane-c4", "KAT", &["coder"], true, 12000.0)]),
            ),
        ]);
        let f =
            head_plan(&PlanRequest::new("t440cesarops", "coder"), &view, &catalogue()).unwrap();
        assert_eq!(f.host_key, "t440cesarops");
        assert_eq!(f.slot, "coder-lane-a");
        assert_eq!(f.reason, "local plan satisfies this role");
    }

    #[test]
    fn remote_fragment_names_a_different_host_and_never_the_asker() {
        // The asker has nothing; the head must not hand it its own host back.
        let view = view_with(vec![
            ("t440cesarops", host(vec![])),
            (
                "cesarops4om",
                host(vec![slot("coder-lane-c4", "KAT", &["coder"], true, 12000.0)]),
            ),
        ]);
        let f =
            head_plan(&PlanRequest::new("t440cesarops", "coder"), &view, &catalogue()).unwrap();
        assert_eq!(f.host_key, "cesarops4om");
        assert_ne!(f.host_key, f.node_id);
        assert!(f.reason.contains("fleet-wide"), "{}", f.reason);
    }

    #[test]
    fn locked_host_is_never_offered() {
        let mut locked = host(vec![slot("coder-lane-c4", "KAT", &["coder"], true, 12000.0)]);
        locked.locked = true;
        let view = view_with(vec![("t440cesarops", host(vec![])), ("cesarops2", locked)]);
        let err = head_plan(&PlanRequest::new("t440cesarops", "coder"), &view, &catalogue())
            .expect_err("a locked host is not an answer");
        assert_eq!(err.reason_token(), "escalate-nowhere");
        assert_eq!(err.http_status(), Some(403));
        let constraint = match &err {
            PlanReject::EscalateNowhere { constraint } => constraint.clone().unwrap(),
            other => panic!("expected escalate-nowhere, got {other:?}"),
        };
        assert!(constraint.contains("cesarops2"), "{constraint}");
        assert!(constraint.contains("locked"), "{constraint}");
    }

    #[test]
    fn unhealthy_slot_is_never_offered() {
        let view = view_with(vec![
            ("t440cesarops", host(vec![])),
            (
                "cesarops4om",
                host(vec![slot("coder-lane-c4", "KAT", &["coder"], false, 12000.0)]),
            ),
        ]);
        let err = head_plan(&PlanRequest::new("t440cesarops", "coder"), &view, &catalogue())
            .expect_err("an unhealthy slot is not an answer");
        assert_eq!(err.reason_token(), "escalate-nowhere");
    }

    #[test]
    fn unsatisfiable_role_is_403_never_an_empty_fragment() {
        // The whole point: no fabricated success. An empty fleet is not a
        // constraint claim either, so no constraint is invented.
        let err = head_plan(
            &PlanRequest::new("t440cesarops", "coder"),
            &view_with(vec![]),
            &catalogue(),
        )
        .expect_err("nothing in the fleet can take this");
        assert_eq!(err, PlanReject::EscalateNowhere { constraint: None });
        assert_eq!(err.http_status(), Some(403));
        assert!(err.to_string().starts_with("escalate-nowhere"), "{err}");
    }

    #[test]
    fn fragment_does_not_leak_the_rest_of_the_fleet() {
        // Neither case may leak the head's full plan: the answer names one
        // host, one slot, one model. Nothing else from the view may appear.
        let view = view_with(vec![
            ("t440cesarops", host(vec![])),
            (
                "cesarops4om",
                host(vec![slot("coder-lane-c4", "KAT", &["coder"], true, 12000.0)]),
            ),
            (
                "cesarops3",
                host(vec![slot("reviewer-e4b", "E4B", &["reviewer"], true, 6000.0)]),
            ),
            (
                "cesarops2",
                host(vec![slot("thinker-nim", "NIM", &["thinker"], true, 2000.0)]),
            ),
        ]);
        let f =
            head_plan(&PlanRequest::new("t440cesarops", "coder"), &view, &catalogue()).unwrap();
        let wire = serde_json::to_string(&f).unwrap();
        for other in ["cesarops3", "cesarops2", "reviewer-e4b", "thinker-nim", "E4B"] {
            assert!(!wire.contains(other), "fragment leaked {other}: {wire}");
        }
        // The fragment type itself has no field that could carry a full plan.
        assert_eq!(
            serde_json::to_value(&f).unwrap().as_object().unwrap().len(),
            FRAGMENT_FIELDS.len()
        );
    }


    // ── F5's swappable planner, on the Rust side ─────────────────────────────

    #[test]
    fn both_planners_satisfy_the_same_fragment_contract() {
        // Same contract, structurally: a fragment from either impl carries the
        // same fields. This is what makes "one interface" a fact, not a claim.
        let view = view_with(vec![
            (
                "t440cesarops",
                host(vec![slot("coder-lane-a", "Ornith-35B", &["coder"], true, 4000.0)]),
            ),
            (
                "cesarops4om",
                host(vec![slot("coder-lane-c4", "KAT", &["coder"], true, 12000.0)]),
            ),
        ]);
        let ask = PlanRequest::new("t440cesarops", "coder");
        let keys = |kind: &str| -> Vec<String> {
            wire_keys(&serde_json::to_value(
                get_planner(kind).unwrap().plan(&ask, &view).unwrap(),
            )
            .unwrap())
        };
        assert_eq!(keys("monolithic"), keys("federated"));
        assert_eq!(keys("monolithic"), expected_fragment_keys());
        assert_eq!(get_planner("monolithic").unwrap().name(), "monolithic");
        assert_eq!(get_planner("federated").unwrap().name(), "federated");
    }

    #[test]
    fn federated_declines_when_only_a_remote_host_could_serve() {
        // The federated planner consults only the asker's own slots, so a
        // remote-only role is `None` — not a fragment pointing elsewhere.
        let view = view_with(vec![
            ("t440cesarops", host(vec![])),
            (
                "cesarops4om",
                host(vec![slot("coder-lane-c4", "KAT", &["coder"], true, 12000.0)]),
            ),
        ]);
        let ask = PlanRequest::new("t440cesarops", "coder");
        assert!(get_planner("federated").unwrap().plan(&ask, &view).is_none());
        assert!(get_planner("monolithic").unwrap().plan(&ask, &view).is_some());
    }

    #[test]
    fn monolithic_never_returns_the_asking_node() {
        let view = view_with(vec![(
            "t440cesarops",
            host(vec![slot("coder-lane-a", "Ornith-35B", &["coder"], true, 4000.0)]),
        )]);
        let ask = PlanRequest::new("t440cesarops", "coder");
        assert!(get_planner("monolithic").unwrap().plan(&ask, &view).is_none());
    }

    #[test]
    fn get_planner_rejects_an_unknown_kind_rather_than_defaulting() {
        let err = get_planner("moe-magic").unwrap_err();
        assert_eq!(err, PlanReject::UnknownPlanner { kind: "moe-magic".into() });
        assert_eq!(err.reason_token(), "bad-enum");
        assert_eq!(PLANNER_KINDS.to_vec(), vec!["monolithic", "federated"]);
    }

    #[test]
    fn serves_mirrors_the_python_rule() {
        // unhealthy -> never; empty roles + a model -> any role it can hold;
        // empty roles + no model -> nothing; declared roles -> membership.
        assert!(!slot("s", "KAT", &["coder"], false, 9000.0).serves("coder"));
        assert!(slot("s", "KAT", &[], true, 1.0).serves("anything"));
        assert!(!slot("s", "", &[], true, 1.0).serves("anything"));
        assert!(slot("s", "KAT", &["coder", "reviewer"], true, 1.0).serves("reviewer"));
        assert!(!slot("s", "KAT", &["coder"], true, 1.0).serves("thinker"));
    }


    // ── the FLEET-scope scoring term ─────────────────────────────────────────

    fn matrix(rows: Vec<(&str, Vec<(&str, Option<f64>)>)>) -> LatencyMatrix {
        LatencyMatrix {
            rpc_median_ms: rows
                .into_iter()
                .map(|(from, tos)| {
                    (
                        from.to_string(),
                        tos.into_iter()
                            .map(|(to, v)| (to.to_string(), v))
                            .collect::<BTreeMap<String, Option<f64>>>(),
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn latency_term_breaks_a_headroom_tie() {
        // Equal free VRAM, different measured RTT: the nearer host wins. This
        // is the bullet — the matrix feeds the FLEET-scope scoring term.
        let view = view_with(vec![
            ("t440cesarops", host(vec![])),
            (
                "cesarops3",
                host(vec![slot("coder-lane-c3", "KAT", &["coder"], true, 8000.0)]),
            ),
            (
                "cesarops4om",
                host(vec![slot("coder-lane-c4", "KAT", &["coder"], true, 8000.0)]),
            ),
        ])
        .with_latency(matrix(vec![(
            "t440cesarops",
            vec![("cesarops3", Some(3.004)), ("cesarops4om", Some(0.501))],
        )]));
        let f =
            head_plan(&PlanRequest::new("t440cesarops", "coder"), &view, &catalogue()).unwrap();
        assert_eq!(f.host_key, "cesarops4om");
        assert_eq!(f.rtt_ms, Some(0.501));
        assert!(f.reason.contains("rtt 0.501 ms"), "{}", f.reason);
    }

    #[test]
    fn a_far_host_still_wins_when_it_has_enough_more_headroom() {
        // The weight is a ranking term, not a veto: 2 GiB more free VRAM beats
        // a 3 ms link at 512 MiB/ms.
        let view = view_with(vec![
            ("t440cesarops", host(vec![])),
            (
                "cesarops3",
                host(vec![slot("coder-lane-c3", "KAT", &["coder"], true, 10000.0)]),
            ),
            (
                "cesarops4om",
                host(vec![slot("coder-lane-c4", "KAT", &["coder"], true, 8000.0)]),
            ),
        ])
        .with_latency(matrix(vec![(
            "t440cesarops",
            vec![("cesarops3", Some(3.0)), ("cesarops4om", Some(0.5))],
        )]));
        let f =
            head_plan(&PlanRequest::new("t440cesarops", "coder"), &view, &catalogue()).unwrap();
        assert_eq!(f.host_key, "cesarops3");
        assert_eq!(fleet_score(10000.0, Some(3.0)), 10000.0 - 3.0 * LATENCY_MB_PER_MS);
        assert_eq!(fleet_score(8000.0, None), 8000.0);
    }

    #[test]
    fn an_unmeasured_link_is_never_given_an_invented_rtt() {
        // The live matrix really holds nulls for an unreachable node. `None`
        // stays `None`: the host is still eligible, scored on headroom alone,
        // and the fragment says the term was not applied.
        let view = view_with(vec![
            ("t440cesarops", host(vec![])),
            (
                "cesarops2",
                host(vec![slot("reviewer-c2", "Gemma-26B", &["reviewer"], true, 7000.0)]),
            ),
        ])
        .with_latency(matrix(vec![(
            "t440cesarops",
            vec![("cesarops2", None), ("cesarops4om", Some(0.5))],
        )]));
        assert_eq!(view.latency.rtt_ms("t440cesarops", "cesarops2"), None);
        let f = head_plan(
            &PlanRequest::new("t440cesarops", "reviewer"),
            &view,
            &catalogue(),
        )
        .unwrap();
        assert_eq!(f.host_key, "cesarops2");
        assert_eq!(f.rtt_ms, None);
        assert!(f.reason.contains("rtt not measured"), "{}", f.reason);
    }


    #[test]
    fn matrix_loader_reads_the_live_nauti_fleet_matrix_shape() {
        // The shape /var/lib/nauti/fleet-matrix.json actually has, nulls and
        // the extra top-level keys included.
        let raw = r#"{
          "kind": "nauti_fleet_matrix",
          "generated_at": 1791788280.98,
          "totals": {"gpus": 9, "online": 3},
          "rpc_median_ms": {
            "t440cesarops": {"cesarops2": null, "cesarops3": 3.004,
                             "cesarops4om": 0.501, "t440cesarops": 0.203}
          },
          "icmp_avg_ms_by_path": {"t440cesarops": {"cesarops2": {"lan-10.44": null}}}
        }"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fleet-matrix.json");
        std::fs::write(&path, raw).unwrap();
        let m = LatencyMatrix::load(&path).unwrap();
        assert_eq!(m.rtt_ms("t440cesarops", "cesarops4om"), Some(0.501));
        assert_eq!(m.rtt_ms("t440cesarops", "cesarops3"), Some(3.004));
        assert_eq!(m.rtt_ms("t440cesarops", "cesarops2"), None);
        assert_eq!(m.rtt_ms("cesarops2", "t440cesarops"), None);
    }

    #[test]
    fn fleet_view_flattens_a_nauti_fleet_view_into_a_matrix_row() {
        let value = serde_json::json!({
            "kind": "nauti_fleet_view",
            "from": "t440cesarops",
            "nodes": [
                {"node": "cesarops4om", "online": true,
                 "rpc_rtt_ms": {"samples": 10, "min": 0.4, "median": 0.501, "p95": 0.7}},
                {"node": "cesarops2", "online": false, "rpc_rtt_ms": {"samples": 0}}
            ]
        });
        let m = LatencyMatrix::from_json(&value);
        assert_eq!(m.rtt_ms("t440cesarops", "cesarops4om"), Some(0.501));
        assert_eq!(m.rtt_ms("t440cesarops", "cesarops2"), None);
    }

    #[test]
    fn snapshot_normalisation_accepts_envelope_and_dict_slots() {
        // Mirrors fleet.planner._hosts/._slots: envelope under "nodes"/"hosts"/
        // "fleet" or bare; slots as a list or a {name: {...}} dict; absent
        // "healthy" means healthy.
        let value = serde_json::json!({
            "nodes": {
                "cesarops4om": {"slots": {"coder-lane-c4": {
                    "model_id": "KAT", "roles": ["coder"], "free_mb": 12000.0}}},
                "cesarops3": {"slots": [
                    {"slot": "reviewer-e4b", "model_id": "E4B", "roles": ["reviewer"],
                     "healthy": false, "free_mb": 6000.0}]},
                "cesarops2": {"locked": true, "slots": []}
            }
        });
        let view = FleetView::from_snapshot_json(&value);
        assert_eq!(view.hosts.len(), 3);
        assert_eq!(
            view.best_slot("cesarops4om", "coder").map(|s| s.slot.clone()),
            Some("coder-lane-c4".to_string())
        );
        assert!(view.best_slot("cesarops3", "reviewer").is_none());
        assert!(view.hosts["cesarops2"].locked);
        // A locked host is not a candidate at all.
        let mut locked = host(vec![slot("coder-lane-a", "Ornith", &["coder"], true, 4000.0)]);
        locked.locked = true;
        let view = view_with(vec![("t440cesarops", locked)]);
        assert!(view.best_slot("t440cesarops", "coder").is_none());
    }

    #[test]
    fn a_snapshot_without_slots_does_not_become_the_envelope() {
        // Python: `slots = host_info.get("slots", host_info)` — the fallback is
        // the HOST object. Reading the envelope instead would turn sibling
        // hosts into slots of the first one.
        let value = serde_json::json!({
            "nodes": {
                "t440cesarops": {"locked": false},
                "cesarops4om": {"slots": [{"slot": "coder-lane-c4", "model_id": "KAT",
                                           "roles": ["coder"], "free_mb": 1.0}]}
            }
        });
        let view = FleetView::from_snapshot_json(&value);
        assert!(view.hosts["t440cesarops"].slots.is_empty());
        assert_eq!(view.hosts["cesarops4om"].slots.len(), 1);
    }


    // ── the client, against a real socket ────────────────────────────────────

    /// Serve one canned response on a real loopback socket and hand back the
    /// base URL plus the request the "head" actually received. No mock library:
    /// this is a `TcpListener`, the same bytes a uvicorn head would see.
    fn serve_once(
        status: u16,
        headers: &[(&str, &str)],
        body: &str,
    ) -> (String, std::thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let mut head = format!("HTTP/1.1 {status} OK\r\n");
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
        head.push_str("Content-Type: application/json\r\nConnection: close\r\n\r\n");
        head.push_str(body);
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).into_owned();
            stream.write_all(head.as_bytes()).unwrap();
            stream.flush().unwrap();
            request
        });
        (url, handle)
    }

    #[test]
    fn ask_head_parses_a_live_fragment_and_posts_the_contract() {
        let body = r#"{"node_id":"t440cesarops","role":"coder","host_key":"cesarops4om",
                       "slot":"coder-lane-c4","model_id":"KAT-Coder-V2.5-Dev-Q8_0",
                       "reason":"no local slot for coder; remote host cesarops4om can take it"}"#;
        let (url, handle) = serve_once(200, &[], body);
        let f = ask_head(&url, &PlanRequest::new("t440cesarops", "coder")).unwrap();
        assert_eq!(f.host_key, "cesarops4om");
        assert_eq!(f.slot, "coder-lane-c4");
        let request = handle.join().unwrap();
        assert!(request.starts_with("POST /v1/plan HTTP/1.1\r\n"), "{request}");
        assert!(request.contains("Content-Type: application/json"), "{request}");
        assert!(request.contains("\"node_id\":\"t440cesarops\""), "{request}");
        assert!(request.contains("\"role\":\"coder\""), "{request}");
        assert!(request.contains("Connection: close"), "{request}");
    }

    #[test]
    fn ask_head_maps_403_to_escalate_nowhere_with_the_constraint_header() {
        let (url, handle) = serve_once(
            403,
            &[("X-Escalate-Constraint", "no VRAM estimate for model X")],
            r#"{"detail":"escalate-nowhere"}"#,
        );
        let err = ask_head(&url, &PlanRequest::new("t440cesarops", "reviewer")).unwrap_err();
        assert_eq!(
            err,
            PlanReject::EscalateNowhere {
                constraint: Some("no VRAM estimate for model X".into())
            }
        );
        assert_eq!(err.reason_token(), "escalate-nowhere");
        assert_eq!(err.http_status(), Some(403));
        handle.join().unwrap();
    }

    #[test]
    fn ask_head_falls_back_to_the_json_detail_when_the_header_is_absent() {
        // fleet/api.py omits X-Escalate-Constraint when F3 recorded nothing.
        let (url, handle) = serve_once(403, &[], r#"{"detail":"escalate-nowhere"}"#);
        let err = ask_head(&url, &PlanRequest::new("t440cesarops", "coder")).unwrap_err();
        assert_eq!(
            err,
            PlanReject::EscalateNowhere { constraint: Some("escalate-nowhere".into()) }
        );
        handle.join().unwrap();
    }

    #[test]
    fn ask_head_maps_404_to_unknown_role() {
        let (url, handle) = serve_once(404, &[], r#"{"detail":"unknown role 'bard'"}"#);
        let err = ask_head(&url, &PlanRequest::new("t440cesarops", "bard")).unwrap_err();
        assert_eq!(err, PlanReject::UnknownRole { role: "bard".into() });
        assert_eq!(err.http_status(), Some(404));
        handle.join().unwrap();
    }


    #[test]
    fn ask_head_reads_a_500_as_backend_down_and_a_200_junk_body_as_missing_field() {
        let (url, handle) = serve_once(503, &[], "head is booting");
        let err = ask_head(&url, &PlanRequest::new("t440cesarops", "coder")).unwrap_err();
        assert_eq!(err.reason_token(), "backend-down");
        assert_eq!(err.http_status(), Some(503));
        handle.join().unwrap();

        // 200 with a body that is not a fragment: the contract was not met.
        let (url, handle) = serve_once(200, &[], r#"{"plan":"all of it"}"#);
        let err = ask_head(&url, &PlanRequest::new("t440cesarops", "coder")).unwrap_err();
        assert_eq!(err.reason_token(), "missing-field");
        handle.join().unwrap();
    }

    #[test]
    fn ask_head_reads_a_chunked_body() {
        // uvicorn may chunk when it does not know the length up front.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let _ = stream.read(&mut buf);
            let part = r#"{"node_id":"t440cesarops","role":"coder","host_key":"c4","#;
            let rest = r#""slot":"coder-lane-c4","model_id":"KAT","reason":"fleet-wide"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n\
                 {len:x}\r\n{part}\r\n{len2:x}\r\n{rest}\r\n0\r\n\r\n",
                len = part.len(),
                len2 = rest.len()
            );
            stream.write_all(resp.as_bytes()).unwrap();
            stream.flush().unwrap();
        });
        let f = ask_head(&url, &PlanRequest::new("t440cesarops", "coder")).unwrap();
        assert_eq!(f.host_key, "c4");
        assert_eq!(f.model_id, "KAT");
        handle.join().unwrap();
    }

    #[test]
    fn an_unreachable_head_is_backend_down_not_a_hang() {
        // Port 1 on loopback: refused immediately. A node's escalation path
        // must fail typed, never hang.
        let started = std::time::Instant::now();
        let err = ask_head_with_timeout(
            "http://127.0.0.1:1",
            &PlanRequest::new("t440cesarops", "coder"),
            Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(matches!(err, PlanReject::HeadUnreachable { .. }), "{err:?}");
        assert_eq!(err.reason_token(), "backend-down");
        assert_eq!(err.http_status(), None);
        assert!(started.elapsed() < Duration::from_secs(5), "escalation path hung");
    }

    #[test]
    fn a_non_http_head_url_is_refused_before_dialing() {
        for bad in ["https://head:8080", "head:8080", "http://", "http://head:notaport"] {
            let err = ask_head(bad, &PlanRequest::new("t440cesarops", "coder")).unwrap_err();
            assert!(matches!(err, PlanReject::BadHeadUrl { .. }), "{bad} -> {err:?}");
            assert_eq!(err.reason_token(), "bad-enum");
        }
        assert_eq!(
            parse_head_url("http://head").unwrap(),
            ("head".to_string(), DEFAULT_HEAD_PORT)
        );
        assert_eq!(parse_head_url("http://10.44.0.12:9106/").unwrap().1, 9106);
    }

    // ── the committed fixtures are the runnable agent check ──────────────────

    #[test]
    fn role_catalogue_skips_underscore_comment_keys() {
        let value = serde_json::json!({
            "_provenance": ["a note, not a role"],
            "coder": ["coder-lane-a", "injector"],
            "thinker": ["thinker", "thinker-nim"]
        });
        let cat = RoleCatalogue::from_json(&value);
        assert!(cat.contains("coder"));
        assert!(cat.contains("thinker"));
        assert!(!cat.contains("_provenance"));
        assert_eq!(cat.len(), 2);
        // Array form honours the same rule.
        let list = RoleCatalogue::from_json(&serde_json::json!(["_comment", "coder"]));
        assert_eq!(list.roles(), &["coder".to_string()]);
    }

    #[test]
    fn committed_m11_fixtures_drive_a_real_fragment() {
        // fixtures/ is transcribed from the head's own SLOT_BACKENDS and
        // FALLBACK_CHAIN, so the agent check runs from a clean checkout with
        // no head reachable and nothing invented.
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let view = FleetView::load_snapshot(&repo.join("fixtures/m11-head-slots.json")).unwrap();
        let raw =
            std::fs::read_to_string(repo.join("fixtures/m11-fallback-chain.json")).unwrap();
        let roles = RoleCatalogue::from_json(&serde_json::from_str(&raw).unwrap());
        assert!(roles.contains("coder"));
        assert!(!roles.contains("_provenance"));

        // c4 asks for coder: it holds no coder slot, so the fragment names a
        // different host and the lease-able slot on it.
        let remote =
            head_plan(&PlanRequest::new("cesarops4om", "coder"), &view, &roles).unwrap();
        assert_eq!(remote.host_key, "t440cesarops");
        assert_eq!(remote.slot, "coder-lane-a");
        assert_eq!(remote.model_id, "Ornith-1.0-35B-Q4_K_M");

        // t440 asks for coder: its own slot answers, local-first.
        let local =
            head_plan(&PlanRequest::new("t440cesarops", "coder"), &view, &roles).unwrap();
        assert_eq!(local.host_key, "t440cesarops");
        assert_eq!(local.reason, "local plan satisfies this role");

        // A role whose only slot the head lists as not running is a typed 403,
        // never an empty fragment.
        let err = head_plan(
            &PlanRequest::new("t440cesarops", "translator+reviewer+judge"),
            &view,
            &roles,
        )
        .unwrap_err();
        assert_eq!(err.reason_token(), "escalate-nowhere");
        assert_eq!(err.http_status(), Some(403));

        // cesarops2 is locked in the fixture (c2 was offline), so its reviewer
        // is never offered even though it is healthy on paper.
        let reviewer =
            head_plan(&PlanRequest::new("cesarops3", "reviewer"), &view, &roles).unwrap();
        assert_ne!(reviewer.host_key, "cesarops2");
        assert_eq!(reviewer.host_key, "cesarops4om");

        // A role outside the catalogue is a 404 even though slots exist.
        let err = head_plan(&PlanRequest::new("cesarops4om", "bard"), &view, &roles).unwrap_err();
        assert_eq!(err.reason_token(), "bad-enum");
        assert_eq!(err.http_status(), Some(404));
    }

    #[test]
    fn the_live_latency_matrix_shape_attaches_real_rtts_to_the_fixture() {
        // Same fixture, plus a matrix in the shape /var/lib/nauti/fleet-matrix
        // .json actually writes. The fragment then carries a measured RTT and
        // an unmeasured pair stays None.
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let view = FleetView::load_snapshot(&repo.join("fixtures/m11-head-slots.json"))
            .unwrap()
            .with_latency(matrix(vec![(
                "cesarops4om",
                vec![("t440cesarops", Some(0.501)), ("cesarops2", None)],
            )]));
        let raw =
            std::fs::read_to_string(repo.join("fixtures/m11-fallback-chain.json")).unwrap();
        let roles = RoleCatalogue::from_json(&serde_json::from_str(&raw).unwrap());
        let f = head_plan(&PlanRequest::new("cesarops4om", "coder"), &view, &roles).unwrap();
        assert_eq!(f.rtt_ms, Some(0.501));
        assert!(f.reason.contains("rtt 0.501 ms"), "{}", f.reason);
    }

    // ── plan_by_kind: the CLI cannot drift from the head's order ─────────────

    #[test]
    fn monolithic_kind_is_local_first_like_the_head() {
        // The gap this closes: picking the monolithic planner directly skips
        // the head's local-first step and 403s on a role the asker holds.
        let view = view_with(vec![
            (
                "t440cesarops",
                host(vec![slot("coder-lane-a", "Ornith-35B", &["coder"], true, 4000.0)]),
            ),
            (
                "cesarops4om",
                host(vec![slot("coder-lane-c4", "KAT", &["coder"], true, 12000.0)]),
            ),
        ]);
        let ask = PlanRequest::new("t440cesarops", "coder");
        // The planner alone skips the asker, so it names the remote host even
        // though the asker could take the role itself.
        let direct = MonolithicMoEPlanner.plan(&ask, &view).unwrap();
        assert_eq!(direct.host_key, "cesarops4om");
        // Through `plan_by_kind` the head's local-first step runs first.
        let via_kind = plan_by_kind("monolithic", &ask, &view, &catalogue()).unwrap();
        assert_eq!(via_kind.host_key, "t440cesarops");
        assert_eq!(via_kind.reason, "local plan satisfies this role");
    }

    #[test]
    fn federated_kind_has_no_remote_step() {
        let view = view_with(vec![
            ("t440cesarops", host(vec![])),
            (
                "cesarops4om",
                host(vec![slot("coder-lane-c4", "KAT", &["coder"], true, 12000.0)]),
            ),
        ]);
        let ask = PlanRequest::new("t440cesarops", "coder");
        let err = plan_by_kind("federated", &ask, &view, &catalogue()).unwrap_err();
        assert_eq!(err.reason_token(), "escalate-nowhere");
        // The constraint names the extent that was consulted, so a reader is
        // not told "the fleet has nothing" when only one node was asked.
        match &err {
            PlanReject::EscalateNowhere { constraint } => {
                let c = constraint.clone().unwrap();
                assert!(c.starts_with("federated: "), "{c}");
            }
            other => panic!("expected escalate-nowhere, got {other:?}"),
        }
        // Same ask, monolithic extent: a different host answers.
        let mono = plan_by_kind("monolithic", &ask, &view, &catalogue()).unwrap();
        assert_eq!(mono.host_key, "cesarops4om");
    }

    #[test]
    fn plan_by_kind_validates_before_planning_and_rejects_unknown_kinds() {
        let view = view_with(vec![(
            "cesarops4om",
            host(vec![slot("coder-lane-c4", "KAT", &["coder"], true, 1.0)]),
        )]);
        // Unknown role is a 404 whatever the kind.
        let err = plan_by_kind(
            "federated",
            &PlanRequest::new("cesarops4om", "bard"),
            &view,
            &catalogue(),
        )
        .unwrap_err();
        assert_eq!(err.http_status(), Some(404));
        // Unknown kind is typed, never a silent default.
        let err = plan_by_kind(
            "moe-magic",
            &PlanRequest::new("cesarops4om", "coder"),
            &view,
            &catalogue(),
        )
        .unwrap_err();
        assert_eq!(err, PlanReject::UnknownPlanner { kind: "moe-magic".into() });
        // A missing field is caught before any planner runs.
        let err = plan_by_kind("monolithic", &PlanRequest::new("", "coder"), &view, &catalogue())
            .unwrap_err();
        assert_eq!(err, PlanReject::MissingField { field: "node_id" });
    }

}

