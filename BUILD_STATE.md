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
