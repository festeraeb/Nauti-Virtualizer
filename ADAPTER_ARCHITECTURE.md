# Adapter Architecture

Adapters translate a leased `Resource` into a concrete attachment. The trait is deliberately small: `name` and `attach`. `Fabric::attach` verifies the resource and active lease before invoking the adapter.

## Adapter Planes

`local-resource` accepts resources registered on the current host and returns a non-mutating local attachment descriptor. `network-resource` accepts resources registered by another node only when the descriptor contains `network.endpoint` and `network.protocol`. It returns a remote attachment descriptor; it does not open a socket, authenticate a peer, or imply that a transport is live.

The next network implementation is a transport adapter that consumes this descriptor and establishes mutual authentication, idempotent requests, renewal, cancellation, health propagation, and cleanup. Iroh/QUIC is the first candidate, but protocol selection remains a tested implementation decision.

Planned adapters:

| Adapter | Upstream base | Custom responsibility |
|---|---|---|
| CPU/NUMA | `sysinfo`, `hwlocality` | Affinity and allocatable CPU-set policy |
| GPU (all-smi) | DRM sysfs + PCI vendor id | Always-discovered, any-brand inventory; NVML is optional enrichment |
| Lemonade | `lemond`/`lemonade` CLI | Reports a Lemonade server's serving state as a resource; AMD/any-brand serving |
| Storage | `sysinfo`, virtio-blk/vhost-user-blk | Volume lifecycle and remote-store semantics |
| Storage | `sysinfo`, virtio-blk/vhost-user-blk | Volume lifecycle and remote-store semantics |
| Network | `rtnetlink`, virtio-net/vhost-user-net | Bandwidth accounting and virtual switch policy |
| VMM | Cloud Hypervisor API/CLI | Idempotent VM/device lifecycle reconciliation |
| VFIO | `vfio-ioctls`/vfio-user | IOMMU validation and least-privilege device assignment |

Adapters must report capabilities and health; they must not bypass lease ownership.

## GPU: all-smi is the authority

`nauti gpus` discovers GPUs from the kernel DRM (`/sys/class/drm/card*`) and attributes
each PCI device to a brand by its vendor id (NVidia 0x10de, AMD 0x1002, Intel 0x8086,
else Unknown). It is **always compiled** — no brand feature gates discovery. `nvidia-smi`
(enrichment via the `nvml-wrapper` library, not the CLI) is optional and only adds VRAM /
NVML facts behind `--features nvidia`; it never gates a card from discovery. Display-only
BMC controllers (e.g. ASPEED `0x1a03`) are collected and excluded from compute resources.
This is fully self-discovering: any brand of GPU reports with no per-host map or script.

## Lemonade: serve any card, ask Lemonade what it is serving

Rather than Nauti driving a GPU it has no driver for, a Lemonade daemon (`lemond`) runs on
the host that owns the card and does the serving (Vulkan backend on consumer AMD, no ROCm).
The `LemonadeAdapter` shells out to the `lemonade` CLI (`status` + `list --downloaded`) and
surfaces a healthy/version/models report. `attach` records the serving endpoint only;
Lemonade owns model lifecycle. **Health is never invented**: an unreachable Lemonade reports
`healthy: false`. See [LEMONADE_ADAPTER.md](LEMONADE_ADAPTER.md) for the type reference and
the verified AMD bring-up runbook (live on `c4` / WX 5100).