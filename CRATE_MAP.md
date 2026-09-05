# Crate Map

| Crate | Version | License | Role | Decision |
|---|---:|---|---|---|
| `sysinfo` | 0.37.2 | MIT | Portable local host inventory | Direct use |
| `serde` / `serde_json` | 1.0.229 / 1.0.149 | MIT/Apache-2.0 | Stable structured Forge output | Direct use |
| `clap` | 4.5.58 | MIT/Apache-2.0 | Forge CLI | Direct use |
| `tracing` | 0.1.41 | MIT | Structured fabric events | Direct use |
| `thiserror` | 2.0.20 | MIT/Apache-2.0 | Typed boundary errors | Direct use |
| `vm-memory` | 0.18.0 | Apache-2.0 OR BSD-3-Clause | Guest-addressable RAM | Direct use |
| `vhost` | 0.14 | Apache-2.0 OR BSD-3-Clause | Vhost protocol | Adapter dependency later |
| `vhost-user-backend` | 0.18 | Apache-2.0 OR BSD-3-Clause | Backend framework | Adapter dependency later |
| `vfio-ioctls` | 0.2 | Apache-2.0 OR BSD-3-Clause | Linux VFIO | Privileged adapter later |
| `iroh` | 0.35 | MIT/Apache-2.0 | Peer-to-peer QUIC transport | Prototype 3 candidate |
| `nvml-wrapper` | 0.11 | MIT | NVIDIA inventory/telemetry | Optional GPU adapter |

Versions not compiled into this workspace are research candidates and must be revalidated before adoption.