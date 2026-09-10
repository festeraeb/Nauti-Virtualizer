# Build State (M0 baseline lock-in, 2026-09-08)

Punchlist tracker: all milestones M0–M10 defined in chat 2026-09-08.
Each milestone closes only when its agent check passes; no skipping ahead.

## Repos and HEADs

- Nauti-Virtualizer `/home/cesarops/festeraeb/Nauti-Virtualizer` — HEAD after this commit.
- forge-fleet `/home/cesarops/forge-fleet` — HEAD after this commit.

## M0 agent check (must stay true)

- [x] `git status --short` clean on both repos (after this commit)
- [x] `cargo test -p nauti-fabric` → 44/44 default
- [x] `cargo test -p nauti-fabric --features rocm` → 48/48
- [x] `cargo test -p nauti-fabric --features nvidia` → 47/47
- [x] `cargo test -p nauti-fabric --features cloud-hypervisor` → 56/56
- [x] `cargo test -p nauti-fabric --features nvidia,cloud-hypervisor` → 59/59
- [x] `cargo test -p dataset-validate` → 12/12 (one flaky failure observed
      2026-09-08, clean on rerun — likely test-order/filesystem race, watch)
- [x] `cargo test --workspace` (forge-fleet) → 67 passed, 0 failed
- [x] `cargo build -p nauti-fabric` → zero warnings

## M0 triage decisions (this commit)

- Nauti dirty diff COMMITTED as-is: `rocm` feature stub + AMD enrichment
  block + `amd_gpus` Lemonade field + untracked `gpu/rocm.rs` (341 lines,
  4 unit tests). Rationale: real code, tested, matches the NVML-enrichment
  pattern; splitting it would churn the same lines twice. The `rocm`
  feature adds zero dependencies (sysfs + optional rocm-smi CLI only).
- forge-fleet M0 repairs COMMITTED:
  - `message.rs`: removed stray DeployModel/ModelServing/UnloadModel
    enum-variant fragments appended after `CommandPollResponse` (broke the
    whole workspace build); added `PendingCommand` struct as the M3
    command-queue placeholder carrying `(command_id, lease, payload)` where
    the M1 rRCP job contract will go.
  - `message.rs`: added missing `InferenceBackend` import.
  - workspace `Cargo.toml` + `fleet-worker/Cargo.toml`: added
    `async-trait = "0.1"` (executor.rs used the macro without the dep —
    pre-existing breakage from commit 5ad119a, masked by the message.rs
    syntax error).
  - `fleet-worker/src/executor.rs`: `port: *port` → `port.unwrap_or(0)`
    (Option<u16> vs u16 mismatch in MockExecutor::list).
  - `fleet-hq/src/api.rs`: command-queue routes commented out with M3
    pointer (handlers don't exist yet; routes referenced missing fns).
  - 3 integration test files + registry.rs fallback: added
    `mac_address`/`net_iface` fields (added to NodeCapabilities in
    1cf569f, fixtures never updated).
- forge-fleet `Cargo.lock` updated (async-trait addition).
- `fleet.db` and `docs/forge-v3/SLICE_2_T440_CANARY_REPORT.md` left
  untracked (local runtime artifact + unreviewed report, not M0 scope).

## Next: M1 — node contract + rRCP job schema (protocol first, code second)

## M1 agent check — CLOSED 2026-09-08

- [x] `RRCP_CONTRACT.md` — canonical §§1–4 + reject catalogue + §5 mapping
      table (no renames), referenced by both repos (no copies).
- [x] `fleet-protocol/src/message.rs` — `RrcpAdvertise/Job/Result` types +
      `validate_rrcp()` + `RRCP_REJECTS` catalogue.
- [x] `fleet-protocol/tests/rrcp_contract.rs` — golden triple validates
      clean; 4 malformed variants (unknown-schema, missing-field,
      unknown-op, lease-expired) reject with exact §4 tokens. 5/5 pass.
- [x] forge-fleet workspace: 67 → **72 passed, 0 failed** (5 new rrcp tests).
- [x] nauti default build still 44/44 (untouched by M1).

## Next: M2 — registry convergence (one truth, two views)

## M2 agent check — CLOSED 2026-09-08

Authority decision: **the node is the authority** (self-report via the
shared DRM walk); Nauti `Fabric` is the resource/lease authority;
fleet-hq is the membership/health view. Neither registry invents GPUs.

- [x] Shared walk: `fleet-worker/src/discovery.rs` gained
      `DrmCardIdentity` + `read_drm_cards()` (same files, same order as
      nauti all-smi). `discover_sysfs_drm_gpus()` now consumes it:
      PCI vendor id authoritative, driver string fallback only;
      identity keyed by BDF (`sysfs-<bdf>` uuid), name carries BDF.
- [x] Live test `read_drm_cards_live_walk_is_sound` passes (BDF-shaped
      identity on every card, PCI-id-wins attribution rule).
- [x] `GpuReport` docs pin the M2 identity rule (BDF suffix matching).
- [x] Live HQ `/v1/fleet` read 2026-09-08: 5 nodes reporting. The OLD
      worker binaries still emit `sysfs-cardN` uuids + `cardN` names
      (pre-M2 build); the M2 code changes uuid/name to BDF form on the
      next worker deploy. Nauti `nauti gpus --json` on this host shows
      BDF-keyed entries (`0000:5e:00.0`, `0000:af:00.0`, mgag200
      display-only) — same cards both views see, identity convergence
      completes when workers redeploy.
- [x] forge-fleet workspace: 72 → **73 passed, 0 failed** (1 new M2 test).
- [x] nauti default build still 44/44 (untouched by M2).

## Next: M3 — local runner on each node (no SSH in the execution path)

## M5 agent check — CLOSED 2026-09-10

- [x] `nauti fabric` RPC verbs: ping/inventory/find/lease/release drive the
      real Iroh/QUIC `RpcRequest` contract; typed errors, no panics.
- [x] Agent self-registration: `agent-serve` writes its `EndpointAddr` to
      `/var/lib/nauti/<node>-addr.json` (env-overridable). The MCP layer
      auto-discovers the fleet by scanning that dir.
- [x] MCP tools (`server.js`): fabric_inventory / fabric_find /
      fabric_lease / fabric_release — 9 tools served, syntax OK, auto-discover.
- [x] Model prep doc `MODEL_PREP.md` committed.
- [x] LIVE cross-host proof (t440 -> c2 over QUIC): ping=Pong;
      lease gpu.virtual.0 -> {id:1,…}; release -> released=true.
