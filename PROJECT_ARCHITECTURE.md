# Project Architecture

Nauti Virtualizer presents a resource-oriented fabric, not a false single-SMP abstraction.

```mermaid
flowchart TD
  LLM[LLM or operator] --> CLI[Forge CLI and structured API]
  CLI --> Control[Policy, placement, leases, audit]
  Control --> Registry[Resource registry and health]
  Control --> Adapter[Adapter registry]
  Adapter --> Local[Local hardware adapters]
  Adapter --> Remote[Remote transport adapters]
  Adapter --> VMM[Cloud Hypervisor and virtio devices]
```

`nauti-fabric` currently implements the portable resource model, local inventory, placement predicates, exclusive leases, adapter authorization, and rust-vmm guest-memory allocation. The control plane stays topology and failure aware; it does not hide remote latency or availability behind an unsafe shared-memory illusion.