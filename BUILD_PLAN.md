# Build Plan

1. Prototype 1: local registry, inventory, exclusive lease, adapter, attachment, release. Complete.
2. Prototype 2: normalized CPU/RAM/storage/network adapters, health states, quota/share semantics, structured CLI/API. In progress.
3. Prototype 3: two-node authenticated agent, remote inventory, renewable leases, heartbeat eviction, and Iroh/QUIC control channel.
4. Prototype 4: Cloud Hypervisor reconciler plus leased virtio or vhost-user device.
5. Prototype 5: topology-aware multi-node placement, GPU/VFIO adapters, and fault-recovery workflows.

The LLM-facing Forge tool contract is designed alongside every stage. A future Strand LoRA adapter creator should generate only adapter skeletons that conform to the lease, capability, health, and test contracts here.