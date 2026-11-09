//! Trust registry: the record of servers an operator has accepted, and the
//! `trust_provider` switch that decides how the fabric uses it.
//!
//! ## Where this fits (Lemonade #1221 / Nexus work)
//!
//! One Ed25519 identity per server is three things at once: the Lemonade
//! `lemond` node key, the Nexus device key, and the Iroh endpoint id this
//! fabric already authenticates in the QUIC handshake. The server's
//! `node_id` is `hex(sha256(pubkey))[..32]`, the same rule Nexus uses for
//! its user ids, so a record written by the Lemonade app and a record read
//! here name the same node.
//!
//! An [`Enrollment`] is the "accept" record. On a trusted LAN it is what the
//! #1221 Accept/Ignore choice writes. In a hierarchical deployment it can
//! carry a [`SponsorSignature`]: the node id of whoever vouched for this
//! key and their signature over the enrollment's [canonical bytes]
//! (`Enrollment::canonical_bytes`). This module stores and round-trips the
//! sponsor signature but does not verify it yet; verification belongs with
//! the branch-head work, and the format needs no change when it arrives.
//!
//! ## The switch
//!
//! [`TrustProvider`] mirrors the `trust_provider` setting in the Lemonade
//! fork:
//!
//! - `none`: home mode. Transport authentication only ([`NoAuth`]).
//! - `nexus`: the Nexus mesh authenticates the link. The fabric still
//!   requires the peer to be an accepted enrollment, so a Nexus member that
//!   nobody accepted cannot lease resources.
//! - `virtualizer`: the fabric's own stack (Iroh endpoint identity) with the
//!   same registry check.
//!
//! `nexus` and `virtualizer` enforce the same registry on purpose: the
//! registry is the single accept record, the stacks differ in transport.
//!
//! ## File format
//!
//! One JSON file, written atomically (temp file, then rename) with mode
//! `0600`. Unknown fields are ignored on read so older and newer writers
//! coexist.
//!
//! [canonical bytes]: Enrollment::canonical_bytes

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::rpc::RpcRequest;
use crate::rpc_auth::{AuthError, AuthProvider, NoAuth, RequestContext};

/// Current on-disk schema version.
pub const REGISTRY_VERSION: u32 = 1;

/// Which trust stack the node runs. Mirrors the fork's `trust_provider`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustProvider {
    /// Home mode: transport authentication only.
    #[default]
    None,
    /// Nexus authenticates the link; the registry still gates the fabric.
    Nexus,
    /// The fabric's own Iroh identity plus the registry.
    Virtualizer,
}

impl fmt::Display for TrustProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TrustProvider::None => "none",
            TrustProvider::Nexus => "nexus",
            TrustProvider::Virtualizer => "virtualizer",
        })
    }
}

impl FromStr for TrustProvider {
    type Err = RegistryError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" | "home" => Ok(TrustProvider::None),
            "nexus" => Ok(TrustProvider::Nexus),
            "virtualizer" | "nauti-virtualizer" => Ok(TrustProvider::Virtualizer),
            other => Err(RegistryError::Invalid(format!(
                "unknown trust_provider {other:?}; expected none, nexus or virtualizer"
            ))),
        }
    }
}

/// The user's choice for a discovered server. `Pending` is a server that
/// was found but not yet decided on; it is never allowed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Admission {
    #[default]
    Pending,
    Accepted,
    Ignored,
}

/// A sponsor's vouching for an enrollment. Stored, not yet verified.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SponsorSignature {
    /// `node_id` of the sponsor.
    pub sponsor_node_id: String,
    /// Hex Ed25519 signature over [`Enrollment::canonical_bytes`].
    pub signature: String,
}

/// One server the operator has decided about.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Enrollment {
    /// `hex(sha256(pubkey))[..32]`; the registry key.
    pub node_id: String,
    /// 64 lowercase hex characters: the Ed25519 public key, which is also
    /// the Iroh endpoint id.
    pub pubkey: String,
    #[serde(default)]
    pub label: String,
    /// Every URL the node announced (one record per node, not per NIC).
    #[serde(default)]
    pub urls: Vec<String>,
    /// Network zone when it was found: `this-machine`, `home`, `tailscale`,
    /// `away`, or anything the writer chooses.
    #[serde(default)]
    pub zone: String,
    #[serde(default)]
    pub admission: Admission,
    /// Milliseconds since the Unix epoch at which the decision was made.
    #[serde(default)]
    pub decided_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sponsor: Option<SponsorSignature>,
}

impl Enrollment {
    /// Bytes a sponsor signs: a fixed, order-stable rendering of the fields
    /// that define the enrollment (not the decision time or the label).
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut urls = self.urls.clone();
        urls.sort();
        format!(
            "nauti-enrollment-v1\nnode_id={}\npubkey={}\nzone={}\nurls={}\n",
            self.node_id,
            self.pubkey,
            self.zone,
            urls.join(",")
        )
        .into_bytes()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("{0}")]
    Invalid(String),
    #[error("registry io: {0}")]
    Io(#[from] std::io::Error),
    #[error("registry json: {0}")]
    Json(#[from] serde_json::Error),
}

/// Normalise a 64-character hex public key (any case) to lowercase.
pub fn normalize_pubkey(s: &str) -> Result<String, RegistryError> {
    let s = s.trim();
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(RegistryError::Invalid(format!(
            "public key must be 64 hex characters (32 bytes), got {} characters",
            s.len()
        )));
    }
    Ok(s.to_ascii_lowercase())
}

/// `hex(sha256(pubkey bytes))[..32]` for a normalised hex public key.
pub fn node_id_from_pubkey(pubkey_hex: &str) -> Result<String, RegistryError> {
    let pk = normalize_pubkey(pubkey_hex)?;
    let mut bytes = [0u8; 32];
    for (i, chunk) in pk.as_bytes().chunks(2).enumerate() {
        bytes[i] = u8::from_str_radix(std::str::from_utf8(chunk).unwrap_or("00"), 16)
            .map_err(|e| RegistryError::Invalid(e.to_string()))?;
    }
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest.iter() {
        out.push_str(&format!("{b:02x}"));
    }
    out.truncate(32);
    Ok(out)
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The set of decided servers, keyed by `node_id`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub trust_provider: TrustProvider,
    #[serde(default)]
    pub entries: BTreeMap<String, Enrollment>,
}

fn default_version() -> u32 {
    REGISTRY_VERSION
}

impl Registry {
    pub fn new() -> Self {
        Self { version: REGISTRY_VERSION, ..Self::default() }
    }

    /// Default file location: `$NAUTI_REGISTRY`, else
    /// `$XDG_CONFIG_HOME/nauti/registry.json`, else `~/.config/nauti/registry.json`.
    pub fn default_path() -> PathBuf {
        if let Ok(p) = std::env::var("NAUTI_REGISTRY") {
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("nauti").join("registry.json")
    }

    /// Read the registry. A missing file is an empty registry; a file that
    /// exists but does not parse is an error (never silently reset).
    pub fn load(path: &Path) -> Result<Self, RegistryError> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let mut reg: Registry = serde_json::from_slice(&bytes)?;
                if reg.version > REGISTRY_VERSION {
                    return Err(RegistryError::Invalid(format!(
                        "registry schema v{} is newer than this build understands (v{})",
                        reg.version, REGISTRY_VERSION
                    )));
                }
                reg.version = REGISTRY_VERSION;
                Ok(reg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// Write atomically with mode 0600.
    pub fn save(&self, path: &Path) -> Result<(), RegistryError> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let tmp = path.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(self)?;
        {
            use std::io::Write;
            #[cfg(unix)]
            use std::os::unix::fs::OpenOptionsExt;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            opts.mode(0o600);
            let mut f = opts.open(&tmp)?;
            f.write_all(&body)?;
            f.write_all(b"\n")?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Record a decision about a server. Idempotent: the same key updates
    /// the existing record (label, URLs, zone, admission) rather than adding
    /// a second one. Returns the `node_id`.
    pub fn enroll(
        &mut self,
        pubkey_hex: &str,
        label: &str,
        urls: &[String],
        zone: &str,
        admission: Admission,
    ) -> Result<String, RegistryError> {
        let pubkey = normalize_pubkey(pubkey_hex)?;
        let node_id = node_id_from_pubkey(&pubkey)?;
        let sponsor = self.entries.get(&node_id).and_then(|e| e.sponsor.clone());
        self.entries.insert(
            node_id.clone(),
            Enrollment {
                node_id: node_id.clone(),
                pubkey,
                label: label.to_string(),
                urls: urls.to_vec(),
                zone: zone.to_string(),
                admission,
                decided_at_ms: now_ms(),
                sponsor,
            },
        );
        Ok(node_id)
    }

    /// Change the decision for a known node. `Err` if the node is unknown.
    pub fn set_admission(&mut self, node_id: &str, admission: Admission) -> Result<(), RegistryError> {
        let e = self
            .entries
            .get_mut(node_id)
            .ok_or_else(|| RegistryError::Invalid(format!("unknown node_id {node_id}")))?;
        e.admission = admission;
        e.decided_at_ms = now_ms();
        Ok(())
    }

    pub fn remove(&mut self, node_id: &str) -> bool {
        self.entries.remove(node_id).is_some()
    }

    /// The enrollment for an Iroh endpoint id string, if any. The endpoint
    /// id is the public key, so it is matched by deriving the node id.
    pub fn find_endpoint(&self, endpoint_id: &str) -> Option<&Enrollment> {
        let node_id = node_id_from_pubkey(endpoint_id).ok()?;
        self.entries.get(&node_id)
    }

    /// True only for an endpoint whose enrollment is `Accepted`.
    pub fn is_accepted(&self, endpoint_id: &str) -> bool {
        self.find_endpoint(endpoint_id).map(|e| e.admission == Admission::Accepted).unwrap_or(false)
    }
}

/// Check an enrollment's sponsor signature against the sponsor's public key.
///
/// `sponsor_pubkey_hex` is 64 hex characters. Fails if the enrollment has no
/// sponsor, if the key does not derive the recorded `sponsor_node_id`, if the
/// signature is not 128 hex characters, or if it does not verify over
/// [`Enrollment::canonical_bytes`].
pub fn verify_sponsor(enrollment: &Enrollment, sponsor_pubkey_hex: &str) -> Result<(), RegistryError> {
    let _ = (enrollment, sponsor_pubkey_hex);
    todo!("implement verify_sponsor")
}

/// What [`Registry::merge`] did.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MergeReport {
    pub added_pending: usize,
    pub updated: usize,
    pub conflicts: Vec<String>,
}

impl Registry {
    /// Merge records reported by another node (a branch head's view).
    ///
    /// A node unknown here is added as `Pending`, whatever the other side
    /// decided: trust is never inherited. A known node keeps its local
    /// admission, label, zone and decision time; its URLs become the sorted,
    /// de-duplicated union of both sides, and it takes the other side's
    /// sponsor only if it has none (counted in `updated` when anything
    /// changed). A record whose `pubkey` does not derive its `node_id`, or
    /// that disagrees with the local `pubkey`, changes nothing and its
    /// `node_id` is listed in `conflicts`.
    pub fn merge(&mut self, other: &Registry) -> MergeReport {
        let _ = other;
        todo!("implement Registry::merge")
    }
}

// ---------------------------------------------------------------------------
// AuthProvider
// ---------------------------------------------------------------------------

/// Allows a request only if its (QUIC-authenticated) peer is an accepted
/// enrollment. A peer that is unknown, pending or ignored is rejected with a
/// reason that says which. The local endpoint is always allowed, so an agent
/// can talk to itself.
///
/// The file is re-read when its modification time changes, so accepting or
/// ignoring a server takes effect on the next request without a restart.
pub struct RegistryAuth {
    name: &'static str,
    path: Option<PathBuf>,
    state: RwLock<Cached>,
}

struct Cached {
    registry: Registry,
    mtime: Option<SystemTime>,
}

impl RegistryAuth {
    /// Provider over a fixed in-memory registry (tests, embedding).
    pub fn from_registry(name: &'static str, registry: Registry) -> Self {
        Self { name, path: None, state: RwLock::new(Cached { registry, mtime: None }) }
    }

    /// Provider that follows a registry file.
    pub fn from_file(name: &'static str, path: PathBuf) -> Result<Self, RegistryError> {
        let registry = Registry::load(&path)?;
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        Ok(Self { name, path: Some(path), state: RwLock::new(Cached { registry, mtime }) })
    }

    fn refresh(&self) {
        let Some(path) = &self.path else { return };
        let current = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let stale = match self.state.read() {
            Ok(s) => s.mtime != current,
            Err(_) => false,
        };
        if !stale {
            return;
        }
        // A file that fails to parse keeps the previous registry in force:
        // a half-written edit must not open or close the fabric by accident.
        if let Ok(reg) = Registry::load(path) {
            if let Ok(mut s) = self.state.write() {
                s.registry = reg;
                s.mtime = current;
            }
        }
    }
}

impl AuthProvider for RegistryAuth {
    fn name(&self) -> &str {
        self.name
    }

    fn authorize(&self, ctx: &RequestContext, _request: &RpcRequest) -> Result<(), AuthError> {
        if ctx.remote_endpoint_id == ctx.local_endpoint_id {
            return Ok(());
        }
        self.refresh();
        let guard = self.state.read().map_err(|_| AuthError::policy("trust registry unavailable"))?;
        match guard.registry.find_endpoint(&ctx.remote_endpoint_id) {
            Some(e) if e.admission == Admission::Accepted => Ok(()),
            Some(e) => Err(AuthError::policy(format!(
                "endpoint {} is {:?} in the trust registry, not accepted",
                short(&ctx.remote_endpoint_id),
                e.admission
            ))),
            None => Err(AuthError::policy(format!(
                "endpoint {} is not enrolled in the trust registry",
                short(&ctx.remote_endpoint_id)
            ))),
        }
    }
}

fn short(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}

/// Build the provider for a `trust_provider` setting.
///
/// `none` never reads the registry file. `nexus` and `virtualizer` require
/// it to parse: a registry that exists but is unreadable is an error, not a
/// silent fall back to open.
pub fn build_provider(
    trust: TrustProvider,
    registry_path: &Path,
) -> Result<Arc<dyn AuthProvider>, RegistryError> {
    Ok(match trust {
        TrustProvider::None => Arc::new(NoAuth),
        TrustProvider::Nexus => Arc::new(RegistryAuth::from_file("nexus", registry_path.to_path_buf())?),
        TrustProvider::Virtualizer => {
            Arc::new(RegistryAuth::from_file("virtualizer", registry_path.to_path_buf())?)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PK_A: &str = "0101010101010101010101010101010101010101010101010101010101010101";
    const PK_B: &str = "0202020202020202020202020202020202020202020202020202020202020202";

    fn ctx(remote: &str) -> RequestContext {
        RequestContext { remote_endpoint_id: remote.to_string(), local_endpoint_id: "local".to_string() }
    }

    #[test]
    fn node_id_is_sha256_prefix_of_the_key_bytes() {
        // sha256 of 32 bytes of 0x01 is a fixed value; pin the first 32 hex chars.
        let id = node_id_from_pubkey(PK_A).unwrap();
        assert_eq!(id.len(), 32);
        assert_eq!(id, node_id_from_pubkey(&PK_A.to_uppercase()).unwrap());
        assert_ne!(id, node_id_from_pubkey(PK_B).unwrap());
        let mut hasher = Sha256::new();
        hasher.update([1u8; 32]);
        let full: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(id, full[..32]);
    }

    #[test]
    fn rejects_malformed_keys() {
        assert!(normalize_pubkey("abc").is_err());
        assert!(normalize_pubkey(&"g".repeat(64)).is_err());
        assert!(node_id_from_pubkey("").is_err());
    }

    #[test]
    fn trust_provider_parses_and_round_trips() {
        for (s, v) in [("none", TrustProvider::None), ("home", TrustProvider::None),
                       ("nexus", TrustProvider::Nexus), ("Virtualizer", TrustProvider::Virtualizer)] {
            assert_eq!(s.parse::<TrustProvider>().unwrap(), v);
        }
        assert!("tailscale".parse::<TrustProvider>().is_err());
        assert_eq!(TrustProvider::Virtualizer.to_string(), "virtualizer");
        let json = serde_json::to_string(&TrustProvider::Nexus).unwrap();
        assert_eq!(json, "\"nexus\"");
    }

    #[test]
    fn enroll_is_idempotent_and_keeps_the_sponsor() {
        let mut r = Registry::new();
        let id = r.enroll(PK_A, "t440", &["http://10.44.0.10:13305".into()], "home", Admission::Pending).unwrap();
        r.entries.get_mut(&id).unwrap().sponsor =
            Some(SponsorSignature { sponsor_node_id: "head".into(), signature: "ab".into() });
        let id2 = r.enroll(PK_A, "t440 renamed", &[], "home", Admission::Accepted).unwrap();
        assert_eq!(id, id2);
        assert_eq!(r.entries.len(), 1);
        let e = &r.entries[&id];
        assert_eq!(e.label, "t440 renamed");
        assert_eq!(e.admission, Admission::Accepted);
        assert!(e.sponsor.is_some(), "re-deciding must not drop the sponsor signature");
    }

    #[test]
    fn only_accepted_endpoints_are_allowed() {
        let mut r = Registry::new();
        r.enroll(PK_A, "a", &[], "home", Admission::Accepted).unwrap();
        r.enroll(PK_B, "b", &[], "home", Admission::Ignored).unwrap();
        let p = RegistryAuth::from_registry("virtualizer", r);
        assert!(p.authorize(&ctx(PK_A), &RpcRequest::Ping).is_ok());
        assert!(p.authorize(&ctx(&PK_A.to_uppercase()), &RpcRequest::Ping).is_ok());
        let ignored = p.authorize(&ctx(PK_B), &RpcRequest::Ping).unwrap_err();
        assert!(format!("{ignored}").contains("Ignored"), "{ignored}");
        let unknown = p.authorize(&ctx(&"ff".repeat(32)), &RpcRequest::Ping).unwrap_err();
        assert!(format!("{unknown}").contains("not enrolled"), "{unknown}");
        // not a key at all
        assert!(p.authorize(&ctx("nonsense"), &RpcRequest::Ping).is_err());
        // the agent itself
        let me = RequestContext { remote_endpoint_id: "x".into(), local_endpoint_id: "x".into() };
        assert!(p.authorize(&me, &RpcRequest::Ping).is_ok());
    }

    #[test]
    fn pending_is_not_allowed() {
        let mut r = Registry::new();
        r.enroll(PK_A, "a", &[], "home", Admission::Pending).unwrap();
        let p = RegistryAuth::from_registry("nexus", r);
        assert!(p.authorize(&ctx(PK_A), &RpcRequest::Ping).is_err());
    }

    #[test]
    fn file_round_trip_and_live_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("registry.json");
        // missing file is empty
        assert!(Registry::load(&path).unwrap().entries.is_empty());
        let mut r = Registry::new();
        r.trust_provider = TrustProvider::Virtualizer;
        let id = r.enroll(PK_A, "a", &["http://a:1".into()], "home", Admission::Accepted).unwrap();
        r.save(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert_eq!(Registry::load(&path).unwrap(), r);

        let p = build_provider(TrustProvider::Virtualizer, &path).unwrap();
        assert_eq!(p.name(), "virtualizer");
        assert!(p.authorize(&ctx(PK_A), &RpcRequest::Ping).is_ok());

        // operator ignores it; the running provider sees it on the next call
        std::thread::sleep(std::time::Duration::from_millis(1100)); // mtime granularity
        let mut r2 = Registry::load(&path).unwrap();
        r2.set_admission(&id, Admission::Ignored).unwrap();
        r2.save(&path).unwrap();
        assert!(p.authorize(&ctx(PK_A), &RpcRequest::Ping).is_err());
    }

    #[test]
    fn a_broken_file_keeps_the_previous_decision_and_fails_closed_at_build() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.json");
        let mut r = Registry::new();
        r.enroll(PK_A, "a", &[], "home", Admission::Accepted).unwrap();
        r.save(&path).unwrap();
        let p = build_provider(TrustProvider::Nexus, &path).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(p.authorize(&ctx(PK_A), &RpcRequest::Ping).is_ok(), "half-written edit must not change policy");
        // a fresh start against the broken file refuses to open
        assert!(build_provider(TrustProvider::Virtualizer, &path).is_err());
        // but home mode never reads it
        assert!(build_provider(TrustProvider::None, &path).is_ok());
    }

    #[test]
    fn none_allows_everyone() {
        let dir = tempfile::tempdir().unwrap();
        let p = build_provider(TrustProvider::None, &dir.path().join("absent.json")).unwrap();
        assert_eq!(p.name(), "none");
        assert!(p.authorize(&ctx("anyone"), &RpcRequest::Ping).is_ok());
    }

    #[test]
    fn newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.json");
        std::fs::write(&path, br#"{"version":99,"entries":{}}"#).unwrap();
        assert!(Registry::load(&path).is_err());
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.json");
        let id = node_id_from_pubkey(PK_A).unwrap();
        let doc = format!(
            r#"{{"version":1,"trust_provider":"nexus","future":true,"entries":{{"{id}":{{"node_id":"{id}","pubkey":"{PK_A}","admission":"accepted","extra":1}}}}}}"#
        );
        std::fs::write(&path, doc).unwrap();
        let r = Registry::load(&path).unwrap();
        assert!(r.is_accepted(PK_A));
        assert_eq!(r.trust_provider, TrustProvider::Nexus);
    }

    #[test]
    fn iroh_endpoint_id_string_matches_the_registry_key() {
        // The auth layer compares the string iroh prints for a peer's id with
        // the registry's pubkey. Prove the two forms are the same bytes.
        let sk = iroh::SecretKey::from_bytes(&[7u8; 32]);
        let pk = sk.public();
        let shown = pk.to_string();
        assert_eq!(shown.len(), 64, "iroh endpoint id Display is 64 hex chars: {shown}");
        let mut r = Registry::new();
        r.enroll(&shown, "iroh", &[], "home", Admission::Accepted).unwrap();
        assert!(r.is_accepted(&shown));
        assert!(r.is_accepted(pk.to_string().as_str()));
    }
}
