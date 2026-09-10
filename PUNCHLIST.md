# Full Build Punchlist (M0–M10)

Sidecar for BUILD_STATE.md — survives context trimming.
Each milestone closes only when its agent check passes; no skipping ahead.

## M0 — Baseline lock-in (no new features)

Triage the dirty Nauti diff (commit / revert / split the `rocm` stub),
`cargo test` matrix green on default + feature sets, both repos push-clean.
Doc: one-paragraph BUILD_STATE.md stating HEADs + dirty/clean.

Agent check: fresh `git status --short` clean on both repos;
`cargo test -p nauti-fabric` passes; `cargo test` in `fleet-protocol` passes.
No M1 work until all three are true.

## M1 — Node contract + rRCP job schema (protocol first, code second)

Single schema doc + matching Rust types: resource advertisement
(converge Nauti Resource/all-smi shape with fleet NodeCapabilities/GpuReport),
job contract (mission id, resources, inputs, outputs, lease, policy scope),
result envelope. rRCP = delivery of that schema, transport-independent.

Agent check: validator accepts a golden advertise→contract→result triple
and rejects 4 malformed variants; both repos reference the same schema file.

## M2 — Registry convergence

Decide authority: Nauti Fabric = resource/lease authority,
fleet-hq = membership/health view. Eliminate dual-registration of same GPUs.

Agent check: `nauti inventory` and HQ `/v1/fleet` agree on GPU count/identity
per node; GPU hot-swap reflects in both within one heartbeat cycle.

## M3 — Local runner on each node (SSH stops being the execution path)

One runner per node that speaks the M1 contract: receives/pulls job,
executes locally, returns result envelope. Retire SSH dispatch for normal path.

Agent check: end-to-end contract in → local execution → result out on c2
with no SSH from Forge during the run; node-offline mid-job produces a typed
failure, not a hang.

## M4 — Forge tools inventory + policy wiring

Audit what tools Forge actually has and where each connects.
Missing tools get built as Forge tools; how they're used is declared in policy.

Agent check: a policy file can enable/disable a tool and change a paddler slot
chain with zero code edits.

## M5 — Warp/model prep (the LLM-facing shell)

Warp gets the resource-aware view: what exists and where, placement/affinity,
health/degradation surfacing. Prep doc for the controlling model.

Agent check: scripted Warp session — ask "what GPUs are free," "load X on a
node with ≥8GB," "show degraded nodes" — all answered from the fabric contract,
zero SSH by the model.

## M6 — VFIO live proof (passthrough stops being mock-only)

VfioGpuAdapter real-ioctl path proven on a real IOMMU host;
--device argv already proven. Least-privilege bind, attach/detach/release cycle,
failure when no IOMMU (typed, honest).

Agent check: live attach → VM sees device → detach → release on c4 or c2 with
IOMMU on; iommu=pt hosts pass; non-IOMMU host returns documented fallback.

## M7 — vhost-user backend proof

One VhostUserBackend impl from the M1 contract (start with RNG/GPIO template,
then one real device: fs or block). Cloud Hypervisor generic add-generic-vhost-user
consumes it; guest sees the device; remote-resource variant proven with a stub.

Agent check: guest-visible device via generic path; kill backend → guest sees
degraded state; remote-stub variant works with backend on a different host.

## M8 — Shared-filesystem story

Either virtio-fs or a declared "NFS-is-the-fabric-store" decision with
lifecycle semantics (mount/attach/detach per lease). No silent third option.

Agent check: a job contract with a filesystem input resolves identically on any
node; lease release detaches cleanly.

## M9 — Fabric VM as a peer domain

Cloud Hypervisor VM running the M3 runner as just another agent behind the same
protocol. Bare-metal pool + VM pool simultaneously; model's view doesn't change.

Agent check: same job contract executes on a bare-metal agent and a VM agent with
identical result envelopes; killing the VM degrades that domain only.

## M10 — Remote-GPU (wgpu-remote) only when a workload needs it

Not before M9. Driven by a real remote-render/compute job, not speculation.

Agent check: workload-defined perf/correctness bar, met or the milestone reopens.

## Build-out order (why this sequence)

Contract (M1) → single truth (M2) → execution without SSH (M3) → policy-governed
tools (M4) → model-facing shell (M5) → hardware depth (M6–M8) → topology
freedom (M9) → exotic transports (M10). Each layer only depends on the one below
it. Skipping ahead recreates the two-fabrics drift.

CXL/DAX/famfs + DRust stay research throughout — tracked, never claimed.
