# Next Actions

1. Add a local agent RPC contract and authenticated Iroh/QUIC two-process proof.
2. Add an adapter capability/health report and lease renewal/expiry tests.
3. Evaluate `hwlocality` for NUMA/PCI capture, gated behind a native dependency feature.
4. Add `nvml-wrapper` behind an NVIDIA feature and test it only on a suitable host.
5. Create a Cloud Hypervisor reconciler proof using a non-destructive local VM and a leased virtio/vhost-user attachment.
6. Define a restricted adapter-creation specification and benchmark dataset before fine-tuning Strand with LoRA.