# Component Map

| Component | Current boundary | Reuse path | Status |
|---|---|---|---|
| Forge API/CLI | `nauti` binary | Clap and JSON | Working local proof |
| Registry/leases | `nauti-fabric` | Custom policy layer | Working local proof |
| Host discovery | `HostInventory` | `sysinfo` | CPU, RAM, disks, NICs |
| Placement | `ResourceRequest` | Custom predicate layer | Local filtering |
| VMM memory | `allocate_guest_memory` | rust-vmm `vm-memory` | Working allocation |
| VMM/device lifecycle | Cloud Hypervisor | Upstream executable/API boundary | Planned adapter |
| Virtio/vhost | rust-vmm / vhost-device | Upstream backends | Planned adapter |
| Remote fabric | Iroh, QUIC or Tailscale TCP | Agent RPC + wgpu-remote job | Live (Iroh agent; M10 Tailscale) |
| GPU | NVML/wgpu-remote/VFIO | Capability adapters | wgpu-remote live over Tailscale; VFIO M6 proven earlier |
| Fleet plan (M11) | `plan.rs` → head `POST /v1/plan` | `fleet/api.py` (F4) owns the decision; the fabric owns the contract, typed rejects and the latency term | Live against the real head (403/404 mapped typed); a live 200 fragment still pending a healthy seat |