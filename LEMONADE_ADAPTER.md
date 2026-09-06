# Lemonade Adapter (AMD & any-brand serving via Lemonade)

The `LemonadeAdapter` lets Nauti treat a **Lemonade server as a serving backend** for a
GPU it does not own the driver for. This is the AMD path: consumer AMD cards (WX 5100,
RX 580) have no first-class `nvidia-smi`; instead a Lemonade daemon (`lemond`) runs on
the host that owns the card, uses Lemonade's **Vulkan** backend (mesa/radv, **no ROCm
installed**), loads a model onto the card, and Nauti just asks Lemonade what it is serving.
It works identically for NVIDIA, Intel, or any brand Lemonade can drive.

## How it fits the fabric

- **all-smi discovery** (`nauti-fabric/src/gpu/discover.rs`) is the single source of truth
  for *what GPUs exist* (DRM sysfs `/sys/class/drm/card*` → PCI vendor id).
- **NVML** (`nvidia` feature) is optional *enrichment* only and never gates discovery.
- **Lemonade** is the *serving* layer: it owns model lifecycle on a card. Nauti's
  `LemonadeAdapter` reports a `LemonadeReport { reachable, version, models[] }` via
  `nauti adapters`, and its `attach` only records the lease + serving `endpoint` —
  it does **not** fabricate health (test `attach_does_not_invent_health`).

## Types (`nauti-fabric/src/adapters.rs`, re-exported from `lib.rs`)

```rust
pub struct LemonadeConfig { host, port, api_key }
pub struct LemonadeAdapter { .. }            // impl ResourceAdapter
pub struct LemonadeReport { reachable, version, models: Vec<LemonadeModel> }
pub struct LemonadeModel { id, backend, downloaded, size_gb }
```

`capability_report()` shells out to the `lemonade` CLI:
`lemonade status` (reachable/version) + `lemonade list --downloaded` (models + backend),
host/port from `--host/--port` or the `LEMONADE_HOST`/`LEMONADE_PORT` env vars. The parser
skips the dashed separator rows in `list --downloaded`. Unreachable servers report
`healthy: false` — never invented.

## Verified AMD bring-up (live on host `c4`, AMD Radeon Pro WX 5100)

End-to-end proof captured with `nauti adapters` on the AMD host (the card actively serves
Bonsai-1.7B via Lemonade Vulkan on device 1 = the WX 5100):

```
NAME            SCOPE            HEALTHY
lemonade        lemonade-serving true
  lemonade.backend   = llamacpp
  lemonade.endpoint  = 127.0.0.1:13306
  lemonade.models    = Bonsai-1.7B-gguf
  lemonade.reachable = true
  lemonade.version   = 11.9.0
```

and `nauti gpus --grouped --type amd` shows the card that backs it:

```
=== AMD (1 devices) ===
AMD  AMD GPU (0000:3b:00.0)  -  8192MB  amdgpu  0000:3b:00.0  -
```

### Runbook (repeat for any AMD host)

1. **Install the binaries** (small, from an existing Lemonade host):
   `lemond` and `lemonade` from `t440` (`/usr/bin/lemond`, `/usr/bin/lemonade`) into
   `/usr/local/bin/`.
2. **Install missing shared libs** (the binaries are not fully static). On Ubuntu 24.04
   copy the real `.so` files from the source host and `ldconfig`:
   `libmbedcrypto.so.2.28.8`, `libcpp-httplib.so.0.26.0`, `libwebsockets.so.19`
   (the last is `lemond`-only) into `/usr/lib/x86_64-linux-gnu/`, create the `*.so.7` /
   `*.so.0.26` soname symlinks, `ldconfig`. `apt install libmbedtls14 …` may cover some.
3. **Copy the server resources catalog** next to the binary:
   `rsync t440:/usr/share/lemonade-server/resources/ /usr/local/bin/resources/`.
   `lemond` looks up `defaults.json` + `server_models.json` relative to the executable.
4. **Pick a free port.** `lemond` defaults to `13305`, but that is commonly taken by an
   existing `llama-server` (on `c4`, the QwQ thinker owns `13305`) — run Lemonade on
   `13306` and point Nauti there via `LEMONADE_PORT`.
5. **The AMD host user must be in group `render`** so it can open
   `/dev/dri/renderD1xx` (mgr `amdgpu`/radv). Verify with `nauti gpus --grouped --type amd`.
6. **Start the daemon** with the Vulkan device pre-selected via env:
   ```bash
   GGML_VK_VISIBLE_DEVICES=<amd-vk-index> lemond --host 127.0.0.1 --port 13306
   ```
   On a multi-GPU host, find the AMD index with
   `~/.cache/lemonade/bin/llamacpp/vulkan/llama-server --list-devices`
   (on `c4`, Vulkan1 = WX 5100; the ASPEED BMC on Vulkan? is display-only and excluded by
   all-smi). `--device` and `-dev` are **reserved** by Lemonade — do not inject them via
   `llamacpp.vulkan_args`; use `GGML_VK_VISIBLE_DEVICES` instead.
7. **Install the Vulkan backend + configure it:**
   ```bash
   lemonade --port 13306 backends install llamacpp:vulkan
   lemonade --port 13306 config set llamacpp.backend=vulkan
   ```
8. **Pull + load a model onto the AMD card:**
   ```bash
   lemonade --port 13306 pull Bonsai-1.7B-gguf
   lemonade --port 13306 load Bonsai-1.7B-gguf --pinned
   ```
   Confirm with `lemonade --port 13306 status` (model row `gpu … ready`) and
   `curl :8001/v1/chat/completions` (the AMD `llama-server` serves on the child port).
9. **Make it durable** — systemd unit (example, `c4`):
   ```ini
   [Service]
   User=cesarops
   Group=cesarops
   SupplementaryGroups=render
   Environment=GGML_VK_VISIBLE_DEVICES=1
   ExecStart=/usr/local/bin/lemond --host 127.0.0.1 --port 13306
   Restart=on-failure
   ```
   A pinned model survives eviction; after a full daemon restart run `load --pinned` again.
10. **Point Nauti at it** and confirm:
    ```bash
    LEMONADE_PORT=13306 nauti adapters          # lemonade healthy: true
    LEMONADE_PORT=13306 nauti adapters --json   # models + backend in detail
    ```

## Notes

- Port `13305` on each host is a serving endpoint owned by the workload (e.g. QwQ). Use a
  dedicated Lemonade port per host (`13306`, …) so adapters don't collide.
- `--device` is a reserved Lemonade argument; steer the GPU with `GGML_VK_VISIBLE_DEVICES`.
- No ROCm is required for consumer AMD; mesa `radv` drives the card through Vulkan.