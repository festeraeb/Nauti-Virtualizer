#!/bin/bash
# Runs inside the jammy CH guest via cloud-init + virtio-fs tag=nauti.
set -u
STORE="${STORE_MNT:-/mnt/nauti}"
RESULTS="$STORE/results"
FLAGS="$STORE/flags"
mkdir -p "$STORE" "$RESULTS" "$FLAGS" 2>/dev/null || true

mount_store() {
  if mountpoint -q "$STORE"; then
    return 0
  fi
  mkdir -p "$STORE"
  for i in $(seq 1 30); do
    if mount -t virtiofs nauti "$STORE" 2>/dev/null; then
      return 0
    fi
    sleep 1
  done
  return 1
}

mount_store || true
mkdir -p "$RESULTS" "$FLAGS"

# --- M7: pin rng_current to the vhost-user instance, not builtin ---
modprobe virtio_rng 2>/dev/null || true
sleep 2
HWRNG_DIR=/sys/devices/virtual/misc/hw_random
ls -l /sys/class/misc/hw_random /dev/hwrng "$HWRNG_DIR" >"$RESULTS/m7-sysfs.txt" 2>&1 || true
cat "$HWRNG_DIR/rng_available" >"$RESULTS/m7-available.txt" 2>&1 || true
cat "$HWRNG_DIR/rng_current" >"$RESULTS/m7-current-before.txt" 2>&1 || true
# Prefer a virtio_rng that is NOT the first/builtin if two exist.
avail=$(cat "$HWRNG_DIR/rng_available" 2>/dev/null || true)
pin=""
for name in $avail; do
  case "$name" in
    virtio_rng.1|virtio_rng.2) pin="$name" ;;
  esac
done
if [ -z "$pin" ]; then
  for name in $avail; do
    case "$name" in
      virtio_rng.*) pin="$name"; break ;;
    esac
  done
fi
if [ -n "$pin" ]; then
  echo "$pin" >"$HWRNG_DIR/rng_current" 2>"$RESULTS/m7-pin.err" || true
fi
cur=$(cat "$HWRNG_DIR/rng_current" 2>/dev/null || echo missing)
echo "$cur" >"$RESULTS/m7-current.txt"
# Publish pin before any blocking hwrng read so the host can kill the backend.
python3 - <<'PY' >"$RESULTS/m7.json" || true
import json, os, time
base="/mnt/nauti/results"
def read(p):
    try:
        return open(os.path.join(base,p)).read().strip()
    except Exception as e:
        return f"ERR {e}"
print(json.dumps({
    "milestone": "M7",
    "rng_available": read("m7-available.txt"),
    "rng_current": read("m7-current.txt"),
    "ts": time.time(),
}, indent=2))
PY
touch "$FLAGS/m7-pinned"
# Prove a live read before the host kills the backend (must not hang firstboot).
timeout 5 dd if=/dev/hwrng of="$RESULTS/m7-before.bin" bs=32 count=1 status=none 2>"$RESULTS/m7-before.err" || true

# Wait for host to kill vhost backend, then try another read (must stall / fail / NEEDS_RESET).
for i in $(seq 1 60); do
  if [ -f "$FLAGS/kill-done" ]; then
    break
  fi
  sleep 1
done
# Non-blocking-ish: timeout so we do not hang firstboot forever.
timeout 8 dd if=/dev/hwrng of="$RESULTS/m7-after.bin" bs=32 count=1 status=none 2>"$RESULTS/m7-after.err"
echo $? >"$RESULTS/m7-after-dd-exit.txt"
cat "$HWRNG_DIR/rng_current" >"$RESULTS/m7-current-after.txt" 2>&1 || true
python3 - <<'PY' >"$RESULTS/m7-after-kill.json" || true
import json, os, time
base="/mnt/nauti/results"
def read(p):
    try:
        return open(os.path.join(base,p)).read().strip()
    except Exception as e:
        return f"ERR {e}"
after=os.path.join(base,"m7-after.bin")
print(json.dumps({
    "milestone": "M7-kill",
    "rng_current": read("m7-current-after.txt"),
    "dd_exit": read("m7-after-dd-exit.txt"),
    "after_bytes": os.path.getsize(after) if os.path.exists(after) else 0,
    "after_err": read("m7-after.err"),
    "ts": time.time(),
}, indent=2))
PY
touch "$FLAGS/m7-done"

# --- M8: same store.path ---
echo "guest-marker $(hostname) $(date -Is)" >"$STORE/guest-marker.txt"
[ -f "$STORE/host-marker.txt" ] && cp "$STORE/host-marker.txt" "$RESULTS/m8-saw-host-marker.txt"
python3 - <<'PY' >"$RESULTS/m8.json" || true
import json, os, time
store="/mnt/nauti"
print(json.dumps({
    "milestone": "M8",
    "store_path": store,
    "guest_marker": open(os.path.join(store,"guest-marker.txt")).read().strip() if os.path.exists(os.path.join(store,"guest-marker.txt")) else None,
    "host_marker": open(os.path.join(store,"host-marker.txt")).read().strip() if os.path.exists(os.path.join(store,"host-marker.txt")) else None,
    "ts": time.time(),
}, indent=2))
PY
touch "$FLAGS/m8-done"

# --- M9: same PeerJob envelope + optional agent-serve from the share ---
if [ -x "$STORE/nauti" ]; then
  "$STORE/nauti" --help >"$RESULTS/m9-nauti-help.txt" 2>&1 || true
  # vsock is the guest/host channel; Iroh still wants a UDP path if net exists.
  nohup "$STORE/nauti" agent-serve --node cesarops2-vm0 >"$RESULTS/m9-agent-serve.log" 2>&1 &
  echo $! >"$RESULTS/m9-agent-pid.txt"
fi
python3 - <<'PY' >"$RESULTS/m9-peerjob.json" || true
import json, time
job = {
    "job_id": "m9-live-1",
    "domain": {"node": "cesarops2-vm0", "kind": "vm", "endpoint": "cid=3,socket=/tmp/nauti-vsock.sock"},
    "op": "echo",
    "store_input": "local.store.fabric",
    "payload": {"store.input": "local.store.fabric", "n": 1},
}
result = {
    "job_id": job["job_id"],
    "domain": job["domain"],
    "status": "ok",
    "output": {"n": 1, "alive": True, "source": "guest"},
}
print(json.dumps({"job": job, "result": result, "ts": time.time()}, indent=2))
PY
# vsock presence
dmesg | grep -i vsock >"$RESULTS/m9-vsock-dmesg.txt" 2>&1 || true
ls -l /dev/vsock >"$RESULTS/m9-vsock-dev.txt" 2>&1 || true
touch "$FLAGS/m9-done"
echo firstboot-ok >"$FLAGS/firstboot-ok"
