# Adapter Creation Specification (v0.1.0)

> **Status:** Frozen for the v0.1.0 LoRA training set. Future versions are additive; breaking changes require a major version bump.
>
> **Audience:** Anyone generating or reviewing Rust adapter code for `nauti-fabric` — humans today, a tuned Strand LoRA tomorrow.

This spec is the contract a generated adapter must satisfy before it is allowed to land in the workspace. It exists because the existing dataset (`training/adapter_creator_dataset.jsonl`) and architecture documents (`ADAPTER_ARCHITECTURE.md`, `TEST_PLAN.md`) describe *principles*, but a model needs a *parseable* list of musts and must-nots. This document is that list.

The spec is intentionally short and restrictive. Adapters that do the minimum — and prove it with tests — are preferred over adapters that do a lot and assert nothing.

---

## 1. Scope

This spec applies to every new `impl ResourceAdapter for ...` in `nauti-fabric/src/`. It does **not** apply to:

- Upstream crates (`vm-memory`, `vhost`, `vhost-user-backend`, `vfio-ioctls`, `iroh`, `nvml-wrapper`, `hwlocality`, `sysinfo`) — they have their own contracts.
- Files under `upstream/` — they are vendored research, not part of the deliverable.
- The CLI binary (`nauti-fabric/src/bin/nauti.rs`) and the RPC layer (`nauti-fabric/src/rpc.rs`) — they consume the adapter trait but are not adapters themselves.

## 2. Required Trait Implementation

Every adapter **must** implement `ResourceAdapter` exactly as declared in `nauti-fabric/src/lib.rs`. No new methods, no blanket `impl` blocks for foreign types.

```rust
impl ResourceAdapter for MyAdapter {
    fn name(&self) -> &str { "my-adapter" }                  // required
    fn attach(&self, resource: &Resource, lease: &Lease)      // required
        -> Result<Attachment, FabricError>;
    fn capability_report(&self) -> AdapterReport { ... }      // required
}
```

A blanket-impl escape hatch (`impl<L: SomeBound> ResourceAdapter for ...`) is allowed **only** if the generated adapter is generic over a behavior trait (e.g. `Launcher` in `vmm::cloud_hypervisor`) and not a foreign concrete type. Generic adapters must compile to a concrete usable type.

## 3. Required Attributes on the Resource

If the adapter reads anything from `resource.attributes`, it **must** declare the keys in the module's doc comment and validate their presence with `MissingResourceAttribute(key)` errors — never panic, never silently default, never invent. See `vmm::cloud_hypervisor::require_attr` for the canonical helper.

| Key prefix | Convention |
|---|---|
| `gpu.*`     | NVIDIA-side facts from NVML (`gpu.name`, `gpu.uuid`, `gpu.pci_bus_id`, `gpu.free_memory_bytes`). |
| `vmm.*`     | VMM-side facts (`vmm.binary`, `vmm.api_socket`, `vmm.kernel`, `vmm.rootfs`, `vmm.vm_name`, `vmm.vcpus`, `vmm.memory_mib`, `vmm.virtio_blk`). |
| `network.*` | Remote-fabric facts (`network.endpoint`, `network.protocol`). |

New key prefixes are allowed but must be documented in this spec and in the module's doc comment before the adapter lands.

## 4. Required `FabricError` Behavior

The adapter **must not** introduce new `FabricError` variants; it must use the existing ones:

- `IncompatibleResourceKind { adapter, expected, actual }` when the resource kind is wrong.
- `IncompatibleResourceLocality { adapter, expected, actual }` when the resource is on the wrong node.
- `MissingResourceAttribute(String)` when a required attribute is absent.
- `AdapterBackendUnavailable { adapter, reason }` (added in v0.1.0) when an external dependency (a binary on PATH, a device file, a kernel module) is missing or returns a non-zero exit. The `reason` string is for the operator, not for routing; do not parse it.
- `GuestMemory(String)` only for `vm-memory` allocation failures (used by the `allocate_guest_memory` free function, not by adapters).
- `ResourceNotFound`, `ResourceUnavailable`, `ResourceAlreadyLeased`, `LeaseNotFound`, `AdapterNotFound` are *fabric-level* errors; an adapter never returns them. If a generated adapter seems to need one, the design is wrong — push the check up into `Fabric::attach`.

Panics (`unwrap()`, `expect()`, indexing without bounds checks) are forbidden in adapter code paths. Use `?` and propagate the typed error.

## 5. Required `capability_report()` Honesty

`capability_report()` **must** return truthful values:

- `healthy` is `true` only if the adapter is currently able to accept an `attach` call. That requires, at minimum, every external dependency (binary on PATH, kernel module loaded, device file present) to be reachable right now. It is not "I would be healthy if you installed X" — that is `healthy: false` with the missing dependency named in `detail`.
- `scope` is a short machine-readable string. Existing values: `"unspecified"`, `"local-host"`, `"remote-descriptor"`, `"vmm-runtime"`. New values are allowed but must be added to the `nauti adapters` documentation.
- `detail` is a `BTreeMap<String, String>` of adapter-specific diagnostics. It is the place to surface "binary is at /usr/bin/cloud-hypervisor" or "GPU 0 UUID is X" so an operator can debug without re-running a probe.

The default trait implementation `scope: "unspecified", healthy: true` is forbidden in any adapter that has external dependencies; override it.

## 6. Required Idempotency

`attach()` **must** be idempotent for `(resource_id, lease_id)`:

- A second call with the same resource and an active lease for it is a **no-op success** returning an `Attachment` that identifies itself as already-running (e.g. `details["already_attached"] = "true"` or equivalent adapter-specific marker).
- It must not call the external side-effect a second time (no second `spawn`, no second `POST`, no second `bind`).
- The "already-running" check must be a **positive signal** from the external system (e.g. a live API socket, a recorded mapping) — not a `then(0)` or a "we tried and it didn't error" guess. See `vmm::cloud_hypervisor::CloudHypervisorAdapter::attach` for the canonical pattern.

A regression test that asserts "first call invokes the side-effect exactly once, second call invokes it zero times" is required (see §8).

## 7. Required Cleanup Path

Every adapter that allocates external state **must** expose a way to free it. The fabric's `release()` only removes the lease; it does **not** call adapter cleanup. The cleanup is the operator's job and lives on the adapter itself.

The contract is:

- The adapter exposes a public cleanup method (e.g. `reconcile()`, `teardown_all()`, `drain()`). The exact name is adapter-specific, but it must be one verb that does the right thing for the whole adapter, not per-lease.
- The cleanup method is **safe to call when no external state exists** (returns an empty list, an empty `Ok`, etc.).
- A test asserts that after `attach(...) → release(lease) → cleanup()` the side-effect was issued exactly once per managed resource.

See `vmm::cloud_hypervisor::CloudHypervisorAdapter::reconcile` and the `reconcile_after_release_issues_exactly_one_remove_per_managed_vm` test for the canonical pattern.

## 8. Required Tests

Every new adapter **must** ship **at least these four tests** in the same module:

1. **Wrong-kind rejection.** A resource of the wrong `ResourceKind` returns `IncompatibleResourceKind`.
2. **Required-attribute rejection.** A resource missing any required attribute returns `MissingResourceAttribute`.
3. **Backend-unavailable rejection.** When the external dependency is missing, `attach` returns `AdapterBackendUnavailable` and `capability_report` returns `healthy: false`.
4. **Idempotency.** The side-effect (recorded by a `MockLauncher` or equivalent) is invoked exactly once on first attach, zero times on second attach with the same resource+lease, and exactly once on cleanup.

Additional tests are encouraged. The "live integration" test (shelling out to the real binary, booting a real VM, etc.) is **strongly encouraged** but must be `#[ignore]` by default and gated on a real host — it is not a substitute for the four unit tests above.

## 9. Required Module Hygiene

- The module **must** be feature-gated if it pulls in a native dependency (`hwlocality`, `nvml-wrapper`, …). The default build must remain dependency-free.
- The module **must** export its public types from `lib.rs` under a `#[cfg(feature = "...")]` attribute.
- The module **must** have a top-of-file doc comment that cites this spec by name and points to the upstream sources it consumes.
- Generated code **must not** copy upstream sources verbatim. Wrapping an upstream API in a small adapter is fine; reimplementing the upstream API to "control it" is not.

## 10. Forbidden Patterns

| Pattern | Why it is forbidden |
|---|---|
| `unwrap()` / `expect()` on adapter inputs | Hides contract violations; tests must drive the failure. |
| New `FabricError` variants in adapter code | The error type is the crate's public contract; additions are the maintainers' call. |
| Reporting `healthy: true` for an adapter whose external dependency is missing | Operators use the report to gate automation; a false `true` is worse than a hard error. |
| Implicit retry loops inside `attach` | Retry is a transport-level concern; the adapter should be deterministic. |
| `unsafe` blocks | This codebase is `#![forbid(unsafe)]` by convention; adapters stay safe. |
| Hard-coded absolute paths in tests | Tests must run on any host. Use a `MockLauncher` or a tempdir. |
| Default-to-false for `exclusive` | `Resource` defaults `exclusive = false`; an adapter that requires exclusivity must set it on the resource at registration time, not silently accept a shared resource. |
| Hard-coding a `chrono::Utc::now()` or `Instant::now()` outside tests | The fabric owns time; adapters consume leases, they don't make them. |

## 11. Grounding Field (Dataset Rule)

Every record added to `training/adapter_creator_dataset.jsonl` after this spec lands **must** include a top-level `grounding` field whose value is a relative path to a file in the repository (e.g. `"nauti-fabric/src/vmm/cloud_hypervisor.rs"`). The dataset validator (`scripts/validate_dataset.py`) refuses records without a `grounding` field that points to an existing file. The nine pre-spec records in the dataset are marked `"grounding": "legacy"` and are grandfathered; new records are not.

## 12. Versioning

- **Patch (0.1.x):** Clarifications, additional examples, additional forbidden patterns. No rule changes.
- **Minor (0.x.0):** New required tests, new required attributes, new `FabricError` variants in the public type. Adapters shipped under the previous minor version are still considered compliant until the next minor bump.
- **Major (x.0.0):** Removal of a forbidden pattern, change of a `must` to a `should`, change of a default. Adapters must be re-certified.

## 13. Self-Reference

This spec is enforced by:

- The reviewer (human) reading the diff.
- `scripts/validate_dataset.py` rejecting new dataset records without a valid `grounding` field.
- `nauti-fabric/tests/dataset_validation.rs` running the validator as part of `cargo test --workspace`.
- The existing adversarial prompts in `training/adapter_creator_dataset.jsonl`, which the dataset expansion for this version extends.
