# Adapter Architecture

Adapters translate a leased `Resource` into a concrete attachment. The trait is deliberately small: `name` and `attach`. `Fabric::attach` verifies the resource and active lease before invoking the adapter.

## Adapter Planes

`local-resource` accepts resources registered on the current host and returns a non-mutating local attachment descriptor. `network-resource` accepts resources registered by another node only when the descriptor contains `network.endpoint` and `network.protocol`. It returns a remote attachment descriptor; it does not open a socket, authenticate a peer, or imply that a transport is live.

The next network implementation is a transport adapter that consumes this descriptor and establishes mutual authentication, idempotent requests, renewal, cancellation, health propagation, and cleanup. Iroh/QUIC is the first candidate, but protocol selection remains a tested implementation decision.

Planned adapters:

| Adapter | Upstream base | Custom responsibility |
|---|---|---|
| CPU/NUMA | `sysinfo`, `hwlocality` | Affinity and allocatable CPU-set policy |
| GPU | `nvml-wrapper`, ROCm tooling, wgpu-remote | Inventory normalization, allocation, reset/health behavior |
| Storage | `sysinfo`, virtio-blk/vhost-user-blk | Volume lifecycle and remote-store semantics |
| Network | `rtnetlink`, virtio-net/vhost-user-net | Bandwidth accounting and virtual switch policy |
| VMM | Cloud Hypervisor API/CLI | Idempotent VM/device lifecycle reconciliation |
| VFIO | `vfio-ioctls`/vfio-user | IOMMU validation and least-privilege device assignment |

Adapters must report capabilities and health; they must not bypass lease ownership.