# Test Plan

| Layer | Proof |
|---|---|
| Unit | Resource matching, lease contention/expiry/release, adapter authorization |
| Local integration | `nauti inventory --json`; host inventory validates CPU and RAM |
| VMM | `vm-memory` allocation; later a Cloud Hypervisor boot/device hotplug test |
| Transport | Mutual authentication, idempotent registration, partition and lease expiry |
| Hardware | GPU reset, VFIO/IOMMU validation, NUMA/PCI locality capture |
| End-to-end | Multi-node allocation, attach, workload, failure, cleanup |

Run `cargo test --workspace` and `cargo run -p nauti-fabric --bin nauti -- demo` for the current proof.