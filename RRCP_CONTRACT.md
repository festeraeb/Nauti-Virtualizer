# rRCP v1 — Node Contract + Job Schema

Status: **M1 protocol definition.** This document is the single source of
truth both repos reference. No copies: `fleet-protocol` implements these
shapes as Rust types with a validator test; `nauti-fabric` consumes them
through the same JSON field names (its `Resource`/`ResourceRequest`/
`Lease`/`Attachment` types predate this doc and are mapped in §5, not
renamed).

Design rule: **transport-independent.** The same JSON bytes travel over
the NFS job queue today (`var/fleet-jobs/pending/<node>/*.json`), over
Iroh/QUIC RPC tomorrow, or inside a `PendingCommand.payload`. The schema
does not name a transport. The delivery mechanism is a deployment detail.

## §1 Resource advertisement (`advertise`)

A node self-reports what it has. The worker builds this from local
observation only (all-smi DRM walk for GPUs, sysfs for CPU/RAM, process
table for models). HQ/Forge never invents resources.

```json
{
  "schema": "rrcp.advertise/v1",
  "node": { "node_id": "<uuid>", "hostname": "cesarops2", "ip": "192.168.4.40" },
  "resources": [
    {
      "id": "gpu.0000:03:00.0",
      "kind": "gpu",
      "capacity_mb": 8192,
      "node": "cesarops2",
      "state": "available",
      "exclusive": true,
      "attrs": {
        "gpu.vendor": "NVIDIA",
        "gpu.name": "GeForce RTX 2060 SUPER",
        "gpu.uuid": "GPU-<...>",
        "gpu.pci_bdf": "0000:03:00.0",
        "gpu.driver": "nvidia"
      }
    }
  ],
  "models": [
    {
      "model_id": "Hermes-3-Llama-3.1-8B.Q4_K_M",
      "backend": "llama",
      "state": "ready",
      "port": 5201,
      "gpu_refs": ["gpu.0000:03:00.0"]
    }
  ]
}
```

Field rules:

- `schema` MUST be exactly `rrcp.advertise/v1`. Unknown schema → reject.
- `resources[].id` MUST be stable across reboots for the same slot
  (PCI BDF for GPUs: `gpu.<bdf>`; `cpu.logical`, `memory.bytes`,
  `storage.<mount>` for host resources). A hot-swap is "old id gone,
  new id appeared", never "the same id changed".
- `resources[].kind` ∈ `cpu | gpu | memory | storage | network | device`.
- `resources[].state` ∈ `available | unavailable`.
- `models[].state` ∈ `unloaded | loading | ready | busy | failed`.
  `ready`/`busy` REQUIRE a live process (evidence-based, never intention).
- `models[].gpu_refs` MUST name ids present in the same advertisement.

## §2 Job contract (`job`)

Forge (or policy acting through Forge) asks the fabric to run work. The
target node's local runner receives this, executes with its own
resources, and returns a §3 result. The runner MUST NOT need SSH, a
shell on another host, or any out-of-band channel to execute.

```json
{
  "schema": "rrcp.job/v1",
  "command_id": "<uuid>",
  "lease": {
    "lease_id": "<uuid>",
    "node_id": "<uuid>",
    "resource_ids": ["gpu.0000:03:00.0"],
    "owner": "paddler/reviewer",
    "expires_at_ms": 1780000000000
  },
  "work": {
    "op": "load_model",
    "model_id": "Hermes-3-Llama-3.1-8B.Q4_K_M",
    "port": 5201,
    "backend": "llama",
    "args": ["--ctx-size", "8192"]
  },
  "policy": { "scope": "forge.inference", "max_wall_ms": 300000 }
}
```

Field rules:

- `schema` MUST be exactly `rrcp.job/v1`.
- `command_id` + `lease.lease_id` is the worker's idempotency key:
  re-delivery of the same pair MUST NOT double-execute (at-least-once
  delivery, exactly-once effect).
- `work.op` ∈ `load_model | unload_model | run_command | attach_device`.
  Unknown op → typed reject, never silent skip.
- `lease.expires_at_ms` MUST be in the future at receipt; expired lease →
  reject before touching hardware.
- `policy.scope` MUST be present; the runner refuses scopes it was not
  configured to serve (sandbox rule: a node serves only its scopes).

## §3 Result envelope (`result`)

The runner returns exactly one of these per `command_id`.

```json
{ "schema": "rrcp.result/v1", "command_id": "<uuid>", "status": "ok",
  "detail": { "endpoint": "http://192.168.4.40:5201", "pid": 23113 } }
{ "schema": "rrcp.result/v1", "command_id": "<uuid>", "status": "rejected",
  "reason": "lease-expired | unknown-op | scope-refused | resource-busy" }
{ "schema": "rrcp.result/v1", "command_id": "<uuid>", "status": "failed",
  "reason": "<human-readable>", "retryable": true }
```

Field rules:

- `schema` MUST be exactly `rrcp.result/v1`.
- `status` ∈ `ok | rejected | failed`. `rejected` means "never attempted"
  (safe to retry elsewhere immediately); `failed` means "attempted, did
  not complete" (`retryable` advises the scheduler).
- `detail` is op-specific and free-form; `reason` is REQUIRED for
  `rejected`/`failed` and MUST be machine-stable (kebab-case token
  first, human text after a colon is allowed).

## §4 Reject catalogue (machine-stable `reason` tokens)

`unknown-schema | missing-field | bad-enum | stale-lease | lease-expired |
unknown-op | scope-refused | resource-busy | resource-gone | backend-down |
executor-error`

## §5 Mapping to on-disk types (no renames in M1)

| rRCP field | nauti-fabric | fleet-protocol |
|---|---|---|
| `resources[]` | `Resource` (id/kind/capacity/state/exclusive/attributes) | `GpuReport` + `NodeCapabilities` (converge in M2) |
| `lease` | `Lease` (u64 id) → M2 moves to UUID lease ids | `Reservation` (lease_id UUID, expires_at) |
| `models[]` | `LemonadeReport` (serving view) | `ModelReport` (process-verified view) |
| `job`/`result` | NEW in M1 (no prior type) | `PendingCommand.payload` carries `job`; worker returns `result` |

M1 does not rename or move any existing type. M2 decides the converged
shape; this doc pins the JSON both sides must already speak.

## §6 Agent check (M1 close-out)

`fleet-protocol` validator test holds a golden `advertise → job →
result-ok` triple plus 4 malformed variants (unknown schema, missing
field, unknown op, expired lease) and rejects exactly the malformed
ones with catalogue tokens. Both repos reference this file path.
The controlling model is prepped on §§1–4, never on SSH commands.
