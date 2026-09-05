# Cloud Hypervisor Integration

Cloud Hypervisor is the per-node VMM. It already supplies VM lifecycle, virtio devices, vhost-user devices, VFIO assignment, CPU/memory/PCI hotplug, and migration support on supported hosts.

Forge should own desired-state reconciliation: acquire leases, create/update a VM through Cloud Hypervisor, attach only resources authorized by those leases, watch health, then detach and release. Cloud Hypervisor is not the cross-node scheduler or registry.

First integration check: launch an unprivileged local VM with a leased disk or vhost-user device, then verify detach/release. VFIO and GPU paths require IOMMU, kernel setup, and explicit hardware approval.