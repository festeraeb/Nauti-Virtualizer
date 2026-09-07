# Fleet Session — 2026-09-07

Operational session notes tying the live fleet to this project. No code
changes in this repo; this doc records deployment state, incidents, and the
next integration steps.

## Fleet state at end of session

| Node | Role | State |
|---|---|---|
| t440 | policy core: resident **Ornith-35B** (2×P100 :5200), paddler :8080, fleet-hq | stable |
| c2 | inference (P100) | **crash-looping** — excluded until fixed (see below) |
| c4 | inference (3×GPU + WX 5100 via lemond :13306) | stable after PEF fix |

## c4 incident (resolved) — hidden fan + PEF

c4 hard-reset every ~4–5 min. Not power, not kernel: a **masked fan sensor**
(FAN3) plus BMC **PEF policy** asserting a chassis reset on the missing-fan
event, invisible to SEL/OS. BIOS stayed up because no OS load. Fix: disabled
all 16 PEF policies (`ipmitool pef`); keep PEF off until a real fan is
reinstalled. Lesson: "every-N-minutes reset that survives everything OS-level
⇒ check BMC PEF policies first."

## c2 incident (open) — driver double-DMA → MCE

NVIDIA driver 580.173.02 double-mapped DMA pages through the Intel IOMMU
(`DMAR: DMA PTE for vPFN already set`) → Bank 0x13 uncorrectable MCE → NMI
reset. Upgraded to 580.178.04 (DMAR count 0), but the box still crash-loops —
under investigation. c2 remains excluded from serving.

## Model policy (Warp-first) — see forge repo `docs/WARP_AI_FLEET.md`

Resident **Ornith-35B** on t440 is the always-on model; Warp/agent clients
get `http://<t440>:8080/reviewer/v1/...` (OpenAI-compatible) with the chain:

1. **NIM** (multi-key/model round-robin, 429/529/5xx retry then fall through)
2. **Ornith** (busy_probe via llama.cpp /metrics slot gauges; sticky pin)
3. **Any loaded model** (thinker QwQ c4:13305) — never cold-load anything

Implemented in the forge **paddler** proxy (Rust, fallback chains + rotation
+ sticky pins in `cesarops-paddler/src/main.rs`), config in `paddler.toml`.
Gemma-4 is retired from serving duty.

## Nauti-Virtualizer next steps (this repo)

1. **P100 fleet consolidation** — once tail fans arrive, the P100s move to
   the X11D chassis: 4×P100 all on x16 lanes. Target: VFIO-passthrough VMs
   per GPU via this repo's cloud-hypervisor + VFIO adapter (gaps 1+2 already
   proven live on c4: VM net provisioning + `--device` wiring).
2. **nauti-nodes onboarding** — register t440/c4 as serving peers per
   NAUTI_NODES_ONBOARDING.md once c2 is resolved.
3. **Lemonade/AMD lane** stays as-is (WX 5100 on c4, Bonsai-1.7B pinned).
