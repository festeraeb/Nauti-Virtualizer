# JOB c4 Qwen3-14B — close nauti-fabric M7/M8/M9 docs + one test

Repo files are in /tmp/nauti-review if you cannot write
`/home/cesarops/festeraeb/Nauti-Virtualizer`.

Think, then output JSON then a BUILD_STATE markdown block.

## Facts (already in code)

- `--rng src=…,id=ch-builtin-rng` always (CH v53 cannot omit builtin rng)
- `shared=on` when user_device / fs / vsock
- `--fs`, `--vsock`, `--watchdog` in build_argv
- `store.rs` bind / virtio-fs / explicit nfs; detach on release
- `peer.rs` same envelope bare vs vm
- Remote entropy unit tests exist in `vhost/entropy.rs`
- Live guest rng_current kill-test still NOT run tonight

## You write

1. A `host_inventory_includes_fabric_store` test snippet for `nauti-fabric/src/lib.rs` tests: `HostInventory::discover` has id `local.store.fabric`.
2. BUILD_STATE bullets: M7 close-out (named builtin rng; remote stub unit-tested; live rng_current still open), M8 closed in code (virtio-fs+bind+nfs), M9 closed in contract (vsock + PeerJob). Next live: jammy boot + rng_current + agent-serve in guest.

Return:
```
TEST:
```rust
...
```
BUILD_STATE:
...markdown...
```
