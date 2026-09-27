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
- [x] **Builtin rng named, not omitted**: CH v53 cannot drop the builtin virtio-rng.
      `build_argv` always emits `--rng src=…,id=ch-builtin-rng` (override via
      `vmm.rng_src`). Guest pins the vhost-user instance with
      `/sys/devices/virtual/misc/hw_random/rng_current`. Kill-test attribution
      is `rng_current`, not "CH has no rng".
- [x] **Remote entropy stub**: `vhost/entropy.rs` unit-tests `entropy-serve` +
      `pump` across hosts. Live guest `rng_current` kill-test is the remaining
      jammy-boot proof (not a code gap).
- [x] **Adapter follow-up**: `vmm.user_device` now emits `--generic-vhost-user device_type=…,socket=…,queue_sizes=…` (CH v53 correct path). `vmm.user_device_type` (default "rng") and `vmm.user_device_queue_sizes` (default "1024") attrs supported; test coverage added.

## M8 agent check — CLOSED in code 2026-09-25

Declared decision, no silent third option:

- Guest attach: Cloud Hypervisor virtio-fs (`--fs tag=,socket=`).
- Host / job attach: same directory as a bind path (`store.path`).
- Cross-node: NFS only when `store.kind=nfs` + `store.nfs_export`.

- [x] `store.rs` — `StoreResolution` + `FabricStoreAdapter`; detach on
      `Fabric::release`.
- [x] Host inventory advertises exclusive `local.store.fabric`.
- [x] Same job path for bind and virtio-fs; NFS export is the job path.
- [x] `shared=on` when `--fs` / `--vsock` / `--generic-vhost-user`.
- [x] **LIVE 2026-09-26 on cesarops2** (t440/c4 untouched): virtiofsd
      `--socket-path` + `--shared-dir /var/lib/nauti/store`, CH `--fs tag=nauti`.
      Guest `/mnt/nauti` saw `host-marker`; host saw `guest-marker nauti-jammy`
      at the same `store.path`. `nauti fabric lease/attach/release` on
      `local.store.fabric` → `released=true` (detach). CH v53 rejects qcow
      backing-file overlays (`MaxNestingDepthExceeded`); flattened qcow2 used.

## M9 agent check — CLOSED in contract 2026-09-25; LIVE 2026-09-26

- [x] `peer.rs` — `PeerJob` / `PeerResult` same envelope for `PeerKind::Bare`
      and `PeerKind::Vm`.
- [x] Killing the VM degrades that domain only; bare envelope unchanged.
- [x] Default vsock spec `cid=3,socket=…` for guest `agent-serve`.
- [x] `--vsock` / `--watchdog` / `--cmdline` / `--initramfs` in CH argv.
- [x] **LIVE 2026-09-26**: jammy guest had `/dev/vsock` (cid=3). Same
      `PeerJob` `m9-live-1` / `echo` on cesarops3 bare (`kind=bare`) and
      guest (`kind=vm`). After `ch-remote shutdown` the VM is gone;
      cesarops3 envelope sha256 unchanged
      (`7fe168e7812c6f1f69c12434bebd29fbe455db36c1716b211ef4dae3d2b25b66`).
      Guest `nauti` binary from the 24.04 host did not exec
      (`GLIBC_2.39` missing on jammy 22.04) — vsock + envelope closed;
      in-guest Iroh `agent-serve` needs a jammy-linked nauti.

## M7 live kill-test — CLOSED 2026-09-26 (cesarops2)

- [x] Jammy 22.04.5 CH v53 `state=Running` with `--rng src=/dev/urandom,id=ch-builtin-rng`
      and `--generic-vhost-user device_type=rng,socket=…,queue_sizes=1024`.
- [x] Guest `rng_available` = `virtio_rng.0 virtio_rng.1`; `rng_current`
      pinned to `virtio_rng.1` (vhost). Builtin remains named `ch-builtin-rng`.
- [x] `kill -9` of `nauti vhost serve`: guest `dd if=/dev/hwrng` exit 124
      (timeout), 0 bytes; VM stayed `Running`. First boot also hung the
      kernel `hwrng` thread for 120s on the pinned device — same attribution.

## IOMMU repair node — LIVE 2026-09-26 (c2 head / c3 peer)

- [x] `nauti_iommu_repair.py` + systemd timer 30s on both nodes. No OpenVINO.
- [x] Hypervisor head = `NAUTI_HYPERVISOR_HEAD=cesarops2` (CH + VFIO domain).
      Standing order on c2: iLO `192.168.4.35` ForceRestart, one-shot, 3×30s.
- [x] Peer may only execute that order. c3 `--repair --ask`:
      `hold / head_reachable_peer_does_not_reboot`, Lemonade `vote=false`.
- [x] Head `--repair`: `hold / head_alive_no_orphaned_lease` — group count 0
      this boot is parked (`iommu=pt`, no `intel_iommu=on`), not a reboot.
- [x] Health HTTP `:9106` on LAN `192.168.4.0/22` (ufw opened). Split-brain
      and Lemonade/PXA never reboot.
- [x] Optional CPU bake-off: c3 Lemonade `Qwen3-14B-GGUF` one completion
      (`vote=false`, `repair_path=false`). No PXQ. No P100 steal.

## M10 agent check — CLOSED 2026-09-26 (Tailscale off-routing)

- [x] Workload: 4096-wide f32 add (`gpu/add.wgsl`) on the GPU node.
- [x] Correctness bar: remote output matches CPU reference (`status=ok`).
- [x] Off-routing: `nauti wgpu serve/run` refuse LAN. Live sockets were
      `100.109.55.22:9110` (c2) and `100.76.255.40:38280` (c3).
- [x] Backend: wgpu Vulkan `NVIDIA P106-100`, 1561 ms. Not a scheduler.
- [x] LAN bind/connect on `192.168.4.38:9110` exits 1 with a typed refuse.

## Research memory bucket — TYPED CLOSE 2026-09-26 (not live RAM)

- [x] `nauti research discover` on c2 and c3: `cxl_bus=false`, no DAX, no
      famfs, `drust_present=false`, `claimed_as_live_ram=false`.
- [x] `memory-research` attach is `AdapterBackendUnavailable`. DRust stays
      an OSDI 2024 artifact, not a nauti backend.

## Four-node agent deploy — 2026-09-26 (pre SFP+ c4↔t440)

- [x] `nauti` + `agent-serve` + health `:9106` + repair timer on t440, c2, c3, c4.
- [x] `forge-fleet-worker` restored on c4 (`cesarops4om` Online, 6 GPUs).
- [x] From t440: fabric ping c2/c3/c4/self = Pong. wgpu Tailscale
      `100.72.182.77` → `100.109.55.22:9110` status ok.
- [x] HQ `/v1/fleet`: 4 linux nodes Online (t440, c2, c3, c4om). Paddler :8080
      and Forge v3 :8099 200.
- Hypervisor head remains cesarops2. SFP+ c4↔t440 is a later physical cutover;
  current path is LAN + Tailscale.

## Fleet test + one-fleet deploy — 2026-09-27 (four nodes, SFP+ c4↔t440 live)

- [x] Milestones M0–M10 all closed (M7 PARTIAL superseded by the 2026-09-26
      live kill-test). Remaining follow-up, not a milestone: in-guest Iroh
      `agent-serve` needs a jammy-linked nauti (host binary needs GLIBC_2.39).
- [x] New-hardware adapters (`hwinfo.rs`, sysfs only, always compiled):
      NIC speed/driver/PCI/NUMA/IPv4, `local.rdma.<dev>.<port>` resources
      (RoCE / iWARP / IB, state, rate, GID 0, netdev, NUMA), `local.numa.<n>`
      memory resources, `gpu.numa_node`. `rdma` adapter: lease/attach
      re-reads sysfs, refuses a port that went DOWN, returns the RoCE v2 GID
      index + IPv4. RDMA ports are exclusive (leased like GPUs).
- [x] Hardware seen: t440 BCM57414 25G `rocep1s0f0` ACTIVE (numa0);
      c4 BCM57414 25G `rocep134s0f0` ACTIVE (numa1) + X722 iWARP `iwo1/iwo2`;
      c4 4× P100; c3 WX 5100 + RX 570 + i915; c2 P106 + RTX 2060S + GTX 1070.
- [x] LIVE: t440 → c4 `fabric lease local.rdma.rocep134s0f0.1` → `attach
      --adapter rdma` (10.0.10.204/24, numa1, gid 5) → `released=true`.
- [x] `nauti fleet [--json] [--paths]`: every addr file → RPC RTT
      (min/median/p95), per-path ICMP (25g / lan-10.44 / lan / tailscale),
      rolled-up inventory. Hub timer `cesarops-nauti-fleet-sync` (t440, 5 min,
      cesaropsserver `scripts/nauti_fleet_sync.sh`) shares addr files, runs
      fleet on every node, adds RoCE v2 `ib_write_lat`, writes
      `/var/lib/nauti/fleet-matrix.json`.
- [x] One fleet: 4/4 online, 152 cpus, 12 GPUs, 130 GiB VRAM, 4 RDMA ports
      active. RPC median 0.26–0.7 ms among t440/c2/c4; c3 ~2–3 ms on every
      path (I218-V NIC on c3, not the fabric). RoCE v2 write t440→c4
      typical 3.95–4.15 µs, p99 ~6 µs; send 4.62 µs.
- [x] Tests: `cargo test -p nauti-fabric` 69 + 6 bin;
      `--features cloud-hypervisor,vhost-user,wgpu-remote,vfio,nvidia` 105 + 6;
      `--features numa` 68 + 6; workspace green (dataset-validate temp-file
      race fixed).
- [x] IOMMU limbo recovery on every node (cesaropsserver
      `nauti_iommu_repair.py`): known-good ledger, states unbound /
      fell_off_bus / missing / lost_group, ladder rebind → reset →
      remove_rescan → escalate (3 consecutive polls, ≤3 steps/hour), GPU
      monitors paused around nvidia reset/remove. Head = NAUTI_HYPERVISOR_HEAD
      only (a VM on t440 no longer splits the brain).
- [x] CPU recovery model on every node: `cesarops-nauti-recovery-model`
      (Qwen3-0.6B, llama-server `-ngl 0` :13320) + runbook
      `/var/lib/nauti/iommu-runbook.md`. `--drill` 4/4 on t440, c2, c3, c4.
      Advice only, never votes.
- [x] LIVE limbo drill c2 GTX 1070 `0000:0d:00.0`: unbound → polls 1–2 hold →
      poll 3 rebind (driver still tearing down) → poll 4 reset → healthy,
      model advice matched every step. Finding: `nauti-daemon` monitors held
      /dev/nvidia* and made the nvidia unbind spin; the ladder now pauses them.
- Open: c2 (hypervisor head) boots `iommu=pt` without `intel_iommu=on` →
  0 IOMMU groups, so VFIO passthrough on c2 is parked (ladder still works at
  driver level). Cloud Hypervisor only on t440/c2.

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
