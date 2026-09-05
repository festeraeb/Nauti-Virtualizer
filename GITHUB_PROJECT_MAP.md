# GitHub Project Map

| Project | Repository | Relevance | Reuse boundary |
|---|---|---|---|
| Cloud Hypervisor | `cloud-hypervisor/cloud-hypervisor` | KVM/MSHV VMM, virtio, vhost-user, VFIO, hotplug and migration | Run/configure as a per-node VMM; do not embed its device model |
| rust-vmm | `rust-vmm` organization | Shared VMM primitives | Consume stable crates such as `vm-memory`, `vhost`, and VFIO support |
| vhost-device | `rust-vmm/vhost-device` | Reusable vhost-user device backends | Launch/configure backend processes where supported |
| wgpu-remote | `wgpu-remote/wgpu-remote` | Remote GPU ideas over Iroh/QUIC | Evaluate behind a GPU adapter; not a generic GPU scheduler |
| DRust | OSDI 2024 research artifact | Ownership-aware distributed-memory model | Research input only; validate maintenance and operational fit |

Shallow local source checkouts are kept under `upstream/` for research and excluded from the project deliverable.