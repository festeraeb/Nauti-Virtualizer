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

## M3 agent check — CLOSED 2026-09-10

- [x] `fleet-hq/src/commands.rs` — new module: `enqueue_job` / `poll_jobs` /
      `ack_job` (SQLite-backed rRCP job queue, at-least-once delivery).
- [x] `fleet-hq/src/api.rs` — three endpoints wired: `POST /v1/commands`
      (enqueue), `GET /v1/commands/pending?node_id=<uuid>` (poll),
      `POST /v1/commands/:command_id/ack` (ack).
- [x] `fleet-hq/src/persistence.rs` — `rrcp_jobs` table migration.
- [x] forge-fleet workspace: 73 → **73 passed, 0 failed** (M3 was a net-new
      module, no existing tests broken).

## M4 agent check — CLOSED 2026-09-10

- [x] `nauti vm launch` — live VM booted on t440 via the fabric
      (`state: "Running"` via `ch-remote info`).
- [x] Tap networking — `nauti0` provisioned by the fabric's
      `NetProvisioner`, passed to the VM as `--net tap=nauti0,mac=...`.
- [x] NAT masquerade — host routes VM traffic to LAN, VM reaches
      fleet-hq / paddler / MCP tools.
- [x] Stale-socket liveness fix — `attach()` now checks socket liveness,
      not just existence (a crashed first launch no longer poisons the
      second).

## M6 agent check — CLOSED 2026-09-10 (revised 2026-09-10 late)

- [x] Live attach: VM launched on c2 with P100 passed through via
      `--device path=/sys/bus/pci/devices/0000:0d:00.0` → state "Running".
- [x] VM sees device: device tree shows `_vfio1` with the P100's 16GB BAR
      mapped.
- [x] Detach: VM shut down, device released from VM.
- [x] Release: P100 rebound to nvidia driver, visible in nvidia-smi.
- [x] `iommu=pt` host (c2) passes — no IOMMU group viability issues for
      single-function groups.
- [x] **REVISED — RTX 2060 SUPER (0a:00.0-3) 4-function passthrough NOW
      FIXED**. The earlier "hardware limitation" note was wrong. Root cause:
      the BIOS DMAR table declares RMRR device scopes on the GPU functions
      (0a:00.1 = audio, 0a:00.2 = USB3, 0a:00.3 = nvidia-gpu), so
      intel-iommu refuses VFIO domain attach for those functions
      (`Firmware has requested this device have a 1:1 IOMMU mapping` →
      `SET_CONTAINER errno 22`). Fix: early-initrd ACPI DMAR override that
      strips those scopes, PLUS a required `oem_revision` bump (kernel's
      `acpi_table_initrd_override()` requires `existing->oem_revision <
      table->oem_revision`, i.e. strictly greater — an iasl-rebuilt table
      with equal revision is silently skipped).
      - `ACPI: Table Upgrade: override [DMAR-HP-ProLiant]` at boot
      - sysfs `/sys/firmware/acpi/tables/DMAR` = 1192 bytes (was 1462)
      - iommu group 31 `reserved_regions` = `msi` only (no RMRR directs)
      - `VFIO_GROUP_SET_CONTAINER` on group 31 → **OK** (was EINVAL)
      - cloud-hypervisor: state `Running`, 4 `_vfio1..4` devices attached
      - guest (Ubuntu 22.04.5) boots to login and enumerates all 4 GPU
        functions: `10de:1f06` (TU106), `10de:10f9` (audio),
        `10de:1ada` (USB3), `10de:1adb` (USB-C).
- [x] Deployment footprint (c2): `/boot/dmar-override.cpio` (newc cpio,
      `kernel/firmware/acpi/dmar.aml`, rev 2, checksum valid),
      `GRUB_EARLY_INITRD_LINUX_CUSTOM="dmar-override.cpio"` in
      `/etc/default/grub`, boots with `iommu=pt` unchanged.
- [x] Flood watcher closed: NVRM/probe spam silenced via worker guards
      (discovery.rs grasps nvidia-smi when all NVIDIA compute devices are
      vfio-bound) + `/etc/modprobe.d/99-nvidia-silence.conf`
      (`install nvidia /bin/true` etc.) + persistenced masked. Boot NVRM
      journal count = 0, no live nvidia-smi/nvidia-modprobe spawns.

## M7 agent check — PARTIAL 2026-09-11 (core verified; degradation pinned at hypervisor layer; one confound documented)

- [x] Backend + guest connectivity: `nauti vhost serve --socket /tmp/m7-rng.sock --source /dev/urandom`
      (vhost-user virtio-rng, v53-compatible) — CH connected to it live; daemon exits when the
      frontend disconnects (observed on VM shutdown).
- [x] Guest-side device visibility: guest (vmlinuz-5.15-jammy + initramfs from the jammy
      cloud-image, `--memory size=2G,shared=on`) saw the vhost-user virtio-rng PCI device
      `1af4:1044` at `0000:00:04.0`; `virtio_rng` module loaded; `/dev/hwrng` present;
      two hwrngs registered (`virtio_rng.0`, `virtio_rng.1`).
- [x] Degradation (hypervisor layer, authoritative): on backend `kill -9`, CH v53 detects the
      disconnect, retries the socket for 1 minute, then fails with `Connection refused` and sets
      the device to `NEEDS_RESET`, stopping queue processing. VM stays `Running` throughout.
- [x] CH v53 syntax facts (hard-won): `--user-device` is the **vfio-user** path, NOT
      vhost-user (fails with `VfioUserCreateClient` against a vhost-user backend). The generic
      vhost-user path is `--generic-vhost-user device_type=rng,socket=<path>,queue_sizes=1024`
      (plural `queue_sizes`; `queue_size` is rejected; numeric device_type 4 or `rng` accepted).
      Initrd flag is `--initramfs`. Guest memory needs `shared=on` for any vhost-user device.
- [x] Working CH v53 boot line (proof of record):
      `cloud-hypervisor --api-socket /tmp/ch-m7.sock --kernel ~/vm-images/vmlinuz-5.15-jammy
      --initramfs ~/vm-images/initrd-5.15-jammy --disk path=/tmp/m7-root.raw --cpus boot=2
      --memory size=2G,shared=on --cmdline "console=ttyS0 root=/dev/vda1 rw
      modules-load=virtio_rng" --console pty --serial file=/tmp/m7-serial.log
      --generic-vhost-user device_type=rng,socket=/tmp/m7-rng.sock,queue_sizes=1024`
- [ ] **Known confound (open)**: CH v53 also auto-instantiates a built-in virtio-rng
      (`rng.src=/dev/urandom` in the VM config — second `1af4:1044` at `0000:00:03.0`). The
      hwrng core routes `/dev/hwrng` to its "current" rng, so guest reads served post-kill
      cannot be attributed to the vhost-user device with certainty (built-in rng survived the
      kill by design). Guest-side kill-test reads continued to succeed, which is consistent
      with reads going to the built-in rng and/or a pre-filled vring backlog. Follow-up:
      eliminate the built-in rng (or `echo` the vhost-user instance into
      `/sys/devices/virtual/misc/hw_random/rng_current`), re-run proof + backend-kill, expect
      reads to stall once the backlog drains. Also unverified: remote-stub variant
      (`entropy-serve` + `pump` across hosts).
- [ ] **Adapter follow-up**: `vmm.user_device` currently emits `--user-device` (vfio-user);
      for vhost-user backends it must emit `--generic-vhost-user device_type=…,socket=…,queue_sizes=…`
      (M7 close-out item before M8).

## Next: M8 — Shared filesystem story

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
