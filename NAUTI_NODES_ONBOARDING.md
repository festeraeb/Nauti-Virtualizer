# Onboarding path: nauti-nodes onto the Nauti fabric

> **Layering note (three projects, one stack):**
>
> - **nauti-inferer** — the inference *engine* (GPU serve, kernels, model load
>   path; extracted from `core/cesarops-inference`). Runs *on* resources.
> - **Nauti-Virtualizer** (this repo) — the *substrate*: discovery (all-smi),
>   leases, VMs/VFIO, serving adapters (Lemonade). Provides resources.
> - **nauti-nodes** — the *control plane* (job federation, worker
>   register/heartbeat, OpenAI-compatible API). Schedules *across* resources
>   by consuming this fabric only. It never talks to the engine's GPU path
>   directly, and the engine never implements leases — each layer talks only
>   to its neighbor.

`nauti-nodes` is a real-fleet node registry (SSH-live polling of cesarops2/3/4/t440)
built before this fabric existed. Today its fleet view is live, but its node
identities and inventory are gathered per-host out-of-band. This fabric can
become that node's substrate, so every host it polls is also a leaseable,
virtualizable, serving-ready resource — without replacing nauti-nodes.

## The gap today (what nauti-nodes can't do)

These map 1:1 to nauti-nodes' own documented limitations (synthetic worker
telemetry, hard-coded fallback GPU list, affinity-score node picking, no
identity proof):

| nauti-nodes has | This fabric adds |
|---|---|
| Per-host inventory snapshots (SSH) | **Leases** — exclusive, TTL'd, renewable claims on CPU/GPU/VM |
| Flat GPU name/model lists | **Capability reports** — health, backend availability, honesty (`AdapterBackendUnavailable`, never fabricated) |
| Static topology reads | **Virtualization** — Cloud Hypervisor launch/shutdown/reconcile, VFIO passthrough |
| No serving awareness | **Lemonade adapter** — what models a node is actually serving, any GPU brand |
| SSH as transport | **Iroh/QUIC RPC** with pluggable auth (`NoAuth` → password → TTL/OAuth) |

## How nauti-nodes joins (three increments, no rewrites)

### 1. Consumer (read-only) — replaces per-host polling where an agent runs

Deploy `nauti agent-serve` on each node (same shape as `forge-fleet-worker`):
systemd unit, `--auth password` on the LAN, port 5001. Then nauti-nodes polls
the fabric instead of SSH for any host that has an agent:

```text
nauti-nodes "live fleet" view  →  agent-connect <node>:5001 → Command::Inventory
```

Inventory JSON already carries what nauti-nodes renders (CPUs, memory, GPUs by
vendor, NUMA). Hosts without an agent keep the SSH path — **both transports
coexist**; the fabric is not a hard dependency.

### 2. Actor (lease-aware) — model placement with reservations

`serve_gemma4`, `llama-server`, Lemonade loads and lineup changes go through
`lease_exclusive` before launch, and release on stop. This is what stops two
jobs grabbing the same P100 when lineups change. Scopes come from the
environment the forge agent is spawned under (`NAUTI_NODE`,
`NAUTI_CONTROL_PLANE`, `NAUTI_ALLOW`) — the sandbox is the env, not a shell.

### 3. Peer (VM + serving fabric) — the "3 machines, one machine" endgame

VM networking (`--net`), VFIO `--device` passthrough, and vsock guest
registration let a launched VM appear as another fabric node. At that point
nauti-nodes' fleet view includes VMs and their attached GPUs, served via
Lemonade, all discovered live (all-smi) — no static maps, no per-host scripts.

## Stability contract nauti-nodes can build against

- `Command`/`Response` in `rpc.rs` is additive-only; `Inventory` JSON keeps
  existing fields stable and only adds.
- Default build stays dependency-minimal; every optional backend is a feature
  flag (`nvidia`, `cloud-hypervisor`, `vfio`, `auth-*`, `config`).
- Honesty rule: a backend that is absent reports
  `AdapterBackendUnavailable { adapter, reason }` — never fabricated data.
  nauti-nodes can render that state as "unavailable", not guess.

## What NOT to do

- Do not require the fabric for nauti-nodes' base function (SSH fallback stays).
- Do not parse `nvidia-smi`/`rocm-smi` CLI output — discovery is the all-smi
  sysfs walk; vendor CLIs are enrichment only.
- Do not hardcode hostnames, BDFs, or model lists anywhere — that is the whole
  point (cards get moved; discovery adapts).
