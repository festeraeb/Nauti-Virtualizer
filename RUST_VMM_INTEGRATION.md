# Rust-VMM Integration

The project directly uses `vm-memory` for guest-addressable RAM allocation. This is a real, compiling integration point rather than a copied abstraction.

Adopt additional rust-vmm crates at their correct layer:

| Crate | Intended layer |
|---|---|
| `vm-device`, `vm-allocator` | Custom VMM/device implementation only |
| `vhost`, `vhost-user-backend` | Custom vhost-user backend adapters |
| `vfio-ioctls` | Privileged physical-device adapter |
| `event-manager`, `vmm-sys-util` | Event-driven backend implementation |

Do not add these merely as dependencies: each needs a concrete adapter or backend proof.