# Nauti-Virtualizer

Rust foundations for Forge: a topology- and policy-aware virtual resource fabric that can expose a coherent resource universe to an operator or model without concealing locality and failure domains.

## Current Proof

`nauti-fabric` provides a local resource registry, host inventory, capability filtering, exclusive leases, authorized adapter attachment, structured events, and a rust-vmm guest-memory allocation boundary.

```bash
cargo test --workspace
cargo run -p nauti-fabric --bin nauti -- inventory --json
cargo run -p nauti-fabric --bin nauti -- demo
```

The JSON inventory is the first Forge tool contract. The `demo` command proves register, lease, attach, and release with the local adapter.

## Scope

Cloud Hypervisor, rust-vmm, vhost-device, Iroh/QUIC, VFIO, and hardware-specific GPU adapters are upstream integration targets. This project does not claim to provide a distributed shared-memory system, remote GPU fabric, or production multi-node control plane yet. The build sequence and explicit gaps are in [BUILD_PLAN.md](BUILD_PLAN.md) and [GAP_ANALYSIS.md](GAP_ANALYSIS.md).

The eventual Strand LoRA adapter creator should be constrained by the adapter contract and must produce tests for lease, capability, health, failure, and cleanup behavior. It should generate thin integrations around upstream components, never duplicate a VMM or transport stack.

A LoRA fine-tuning corpus and its curation rules are maintained in a separate fine-tuning repository (not bundled with this crate). A plug-in validation tool for anyone curating a corpus in the same shape lives in `tools/dataset-validate/`; it is built on demand with `cargo run -p dataset-validate -- <path-to-corpus.jsonl>` and is **not** a build or test-time dependency of the deployable crate.
