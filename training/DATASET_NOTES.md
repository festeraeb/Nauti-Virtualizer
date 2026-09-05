# Adapter Creator Dataset Notes

`adapter_creator_dataset.jsonl` is a seed supervised fine-tuning set for a future Strand LoRA. Each record uses the chat `messages` schema and is grounded in the current repository rather than imagined features.

## Truth Sources

| Claim area | Source |
|---|---|
| Resource, lease, and attachment behavior | `nauti-fabric/src/lib.rs` and passing unit tests |
| Local and remote adapter behavior | `nauti-fabric/src/adapters.rs` and passing unit tests |
| Inventory fields | `HostInventory` using `sysinfo` |
| Guest RAM allocation | Direct rust-vmm `vm-memory` integration |
| Cloud Hypervisor and VFIO boundaries | [CLOUD_HYPERVISOR_INTEGRATION.md](../CLOUD_HYPERVISOR_INTEGRATION.md) and [ADAPTER_ARCHITECTURE.md](../ADAPTER_ARCHITECTURE.md) |

## Curation Rules

- Add only claims supported by source, tests, or verified upstream documentation.
- For unfinished work say `unknown`, `planned`, or `research candidate`.
- Keep adversarial prompts that try to bypass leases, make descriptors imply live transport, fabricate topology/capacity, or avoid security prerequisites.
- Do not include secrets, private endpoints, host identifiers, copied upstream source, or unreviewed model output.
- Hold out adapter families and failure cases for evaluation before fine-tuning.

This is a seed corpus, not sufficient training data. Expand it using reviewed adapter pull requests, corrected failures, scrubbed integration test traces, and API contract examples. Evaluate the tuned model against the adversarial prompts before allowing it to generate code changes.