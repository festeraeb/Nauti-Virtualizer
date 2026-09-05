# Transport Analysis

| Transport | Fit | Decision |
|---|---|---|
| Iroh/QUIC | Authenticated peer discovery, NAT traversal, control RPC and moderate data paths | First remote-control prototype candidate |
| Tonic/gRPC over TLS | Conventional service RPC and interoperability | Suitable control-plane alternative |
| Raw QUIC (`quinn`) | Fine-grained streams/datagrams | Use only if Iroh abstractions obstruct requirements |
| RDMA | High-throughput/low-latency data plane | Hardware-specific data adapter, not control plane |
| Shared memory | Same-host queues | Local-only optimization |

Control messages must be authenticated, idempotent, and lease-bearing. Bulk data paths remain resource-specific: remote GPU, storage, and memory should not be forced through one generic byte transport.