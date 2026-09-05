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
| Remote fabric | Iroh, QUIC or RDMA | Thin transport abstraction | Not implemented |
| GPU | NVML/wgpu-remote/VFIO | Capability adapters | Not implemented |