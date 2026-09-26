# JOB c4 — review + BUILD_STATE close-out (no kernel work)

You are a reviewer on cesarops4. Do not invent GPU kernels. Do not touch Burn.

Repo if mounted: look for `Nauti-Virtualizer` / `nauti-fabric`.
If you cannot write the repo, return a JSON review only.

## Read these

- `PUNCHLIST.md` M7–M9
- `BUILD_STATE.md` M7 partial + “Next: M8”
- `nauti-fabric/src/store.rs`
- `nauti-fabric/src/peer.rs`
- `nauti-fabric/src/vmm/cloud_hypervisor.rs` `build_argv` + `VmResourceSpec`

## Return exactly this JSON (no extra prose)

```json
{
  "m7_builtin_rng": "named-id | still-confounded | missing",
  "m7_remote_entropy_tests": "present | missing",
  "m8_decision": "virtio-fs+bind+explicit-nfs | unclear | missing",
  "m8_release_detaches": true,
  "m9_envelope_identity_test": true,
  "argv_missing": ["fs", "vsock", "watchdog"],
  "live_blockers": ["short string"],
  "build_state_patch": "3-8 bullets to append under M7/M8/M9"
}
```

If argv already has fs/vsock/watchdog, `argv_missing` is [].
Honesty rule: if you did not read a file, say so in live_blockers.
