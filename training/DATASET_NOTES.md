# Adapter Creator Dataset Notes

`adapter_creator_dataset.jsonl` is a seed supervised fine-tuning set for a future Strand LoRA. Each record uses the chat `messages` schema and is grounded in the current repository rather than imagined features.

## Machine-Checkable Rules (v0.1.0+)

Every record is validated by `scripts/validate_dataset.py`, which is invoked from `cargo test --workspace` via `nauti-fabric/tests/dataset_validation.rs`. The full contract is in [ADAPTER_CREATION_SPEC.md](../ADAPTER_CREATION_SPEC.md); the short version:

- Every record has a `messages` array with exactly `system / user / assistant` in that order.
- Every record has a top-level `grounding` field. Pre-v0.1.0 records carry `"grounding": "legacy"` and are grandfathered. New records must point to a relative path that exists in the repository.
- `assistant` content must not contain `TBD`, `TODO`, `FIXME`, `unverified`, `placeholder`, `127.0.0.1`, `192.168.`, `10.0.0.`, or the keywords `secret` / `password` / `api_key` / `token`.
- The soft grounding check confirms the assistant answer shares at least one identifier-shaped token with the `grounding` file (catches fully ungrounded answers).

To add a new record: write the JSONL line, set `grounding` to a real file path, run `python3 scripts/validate_dataset.py` locally, and confirm `cargo test -p nauti-fabric` still passes.

## Truth Sources

| Claim area | Source |
|---|---|
| Resource, lease, and attachment behavior | `nauti-fabric/src/lib.rs` and passing unit tests |
| Local and remote adapter behavior | `nauti-fabric/src/adapters.rs` and passing unit tests |
| NVIDIA GPU inventory and projection | `nauti-fabric/src/gpu.rs` and passing unit tests (feature: `nvidia`) |
| Cloud Hypervisor adapter contract | `nauti-fabric/src/vmm/cloud_hypervisor.rs` and passing unit tests (feature: `cloud-hypervisor`) |
| NUMA / PCI topology | `nauti-fabric/src/topology.rs` and passing unit tests (feature: `numa`) |
| Inventory fields | `HostInventory` using `sysinfo` |
| Guest RAM allocation | Direct rust-vmm `vm-memory` integration |
| Adapter-creation rules of engagement | [ADAPTER_CREATION_SPEC.md](../ADAPTER_CREATION_SPEC.md) |
| Cloud Hypervisor and VFIO boundaries | [CLOUD_HYPERVISOR_INTEGRATION.md](../CLOUD_HYPERVISOR_INTEGRATION.md) and [ADAPTER_ARCHITECTURE.md](../ADAPTER_ARCHITECTURE.md) |

## Curation Rules

- Add only claims supported by source, tests, or verified upstream documentation.
- For unfinished work say `unknown`, `planned`, or `research candidate`.
- Keep adversarial prompts that try to bypass leases, make descriptors imply live transport, fabricate topology/capacity, avoid security prerequisites, lie about capability health, or skip required tests behind a feature gate.
- Do not include secrets, private endpoints, host identifiers, copied upstream source, or unreviewed model output.
- Hold out adapter families and failure cases for evaluation before fine-tuning.

This is a seed corpus, not sufficient training data. Expand it using reviewed adapter pull requests, corrected failures, scrubbed integration test traces, and API contract examples. Evaluate the tuned model against the adversarial prompts before allowing it to generate code changes.
