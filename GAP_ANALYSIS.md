# Gap Analysis

Missing implementation is concentrated in the integration layer, not reimplementation of upstream primitives:

- No remote agent, authenticated control transport, distributed consensus, renewable lease persistence, or failure detector.
- No measured NUMA/PCI/network topology, placement scoring, capacity sharing, reservations, or policy engine.
- No NVIDIA/AMD/Vulkan/GPU, VFIO, remote-memory, RDMA, storage, or virtual-network adapters.
- No Cloud Hypervisor reconciliation, virtio/vhost-user backend, VM boot, hotplug, or hot-remove proof.
- No audit store, authentication/authorization, metrics exporter, or production observability pipeline.
- DRust/CXL/DAX/famfs are research directions; none is claimed as a deployable coherent-memory backend here.

These are intentional gaps. Each needs a bounded proof before production design.