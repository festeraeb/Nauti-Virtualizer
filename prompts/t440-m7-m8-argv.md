# JOB t440 — finish Cloud Hypervisor argv + CLI (M7 leftover + M8/M9 flags)

Repo: `/home/cesarops/festeraeb/Nauti-Virtualizer`
Crate: `nauti-fabric`
Do not touch NautiBurn, Forge, or llama.cpp.
Do not rewrite the forward. Do not add crates.

## Already done (do not redo)

- `nauti-fabric/src/store.rs` — FabricStoreAdapter, bind / virtio-fs / nfs
- `nauti-fabric/src/peer.rs` — PeerJob / PeerResult identity
- `lib.rs` — `pub mod store`, `pub mod peer`, `ResourceAdapter::detach`, `Fabric.attachments`, `HostInventory` advertises `local.store.fabric`
- `build_argv` already emits: `shared=on` when user_device/fs/vsock, `--rng src=…,id=ch-builtin-rng`, `--initramfs`, `--cmdline`

## You do this only

File `nauti-fabric/src/vmm/cloud_hypervisor.rs`:

1. In `build_argv`, after the existing `--net` / `--generic-vhost-user` block, emit:
   - `vmm.fs` → `--fs <value>` (CH v53: `tag=…,socket=…`)
   - `vmm.vsock` → `--vsock <value>` (CH v53: `cid=3,socket=…`)
   - `vmm.watchdog` in {on,true} → `--watchdog`
2. Add those three fields (plus `initramfs`, `cmdline`, `rng_src`, `shared`) as `Option<String>` on `VmResourceSpec` and map them in `into_attributes`.
3. Add three unit tests next to the existing vhost-user argv tests:
   - fs spec appears and forces `shared=on`
   - vsock spec appears and forces `shared=on`
   - watchdog flag appears; no-fs launch stays `size=512M` without shared

File `nauti-fabric/src/bin/nauti.rs`:

4. Add optional Launch flags: `--fs`, `--vsock`, `--watchdog`, `--cmdline`, `--initramfs`, `--rng-src`, `--shared`. Wire them into `VmLaunchArgs` / `VmResourceSpec`.
5. Register `FabricStoreAdapter` in `adapters()` and `agent_serve()`.

## Pass bar

```
cd /home/cesarops/festeraeb/Nauti-Virtualizer
cargo test -p nauti-fabric --features cloud-hypervisor
```

Must stay green. No new warnings you introduced.
When done, print the test counts and the files you changed. Stop.
