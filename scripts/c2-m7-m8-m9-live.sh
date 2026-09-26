#!/bin/bash
# Live M7/M8/M9 on cesarops2. Does not touch t440 or c4.
set -euo pipefail
RUN="${RUN_DIR:-/var/lib/nauti/live}"
STORE="${STORE_DIR:-/var/lib/nauti/store}"
IMAGES="${IMAGES_DIR:-$HOME/vm-images}"
CH="${CH_BIN:-/usr/local/bin/cloud-hypervisor}"
NAUTI="${NAUTI_BIN:-/usr/local/bin/nauti}"
VIRTIOFSD="${VIRTIOFSD_BIN:-/usr/libexec/virtiofsd}"
API="$RUN/ch.sock"
RNG_SOCK="$RUN/m7-rng.sock"
FS_SOCK="$RUN/virtiofs.sock"
VSOCK_SOCK="$RUN/nauti-vsock.sock"
SERIAL="$RUN/serial.log"
SEED="$RUN/seed.img"
ROOT="$RUN/jammy-live.qcow2"

mkdir -p "$RUN" "$STORE/results" "$STORE/flags"
# Drop a previous failed live attempt.
for pidfile in "$RUN/ch.pid" "$RUN/vhost-rng.pid" "$RUN/virtiofsd.pid"; do
  if [ -f "$pidfile" ]; then
    kill "$(cat "$pidfile")" 2>/dev/null || true
  fi
done
pkill -f "$RNG_SOCK" 2>/dev/null || true
pkill -f "$FS_SOCK" 2>/dev/null || true
rm -f "$STORE/flags/"* "$API" "$RNG_SOCK" "$FS_SOCK" "$VSOCK_SOCK" "$ROOT"
echo "host-marker $(hostname) $(date -Is)" >"$STORE/host-marker.txt"
install -m 0755 "$(dirname "$0")/guest-firstboot.sh" "$STORE/guest-firstboot.sh"
install -m 0755 "$NAUTI" "$STORE/nauti"

# Writable overlay so we do not mutate the golden qcow.
GOLDEN="$IMAGES/jammy-server-cloudimg-amd64-custom-20241017-0.qcow2"
if [ ! -f "$GOLDEN" ]; then
  echo "missing $GOLDEN" >&2
  exit 1
fi
# CH v53 refuses qcow backing-file nesting (MaxNestingDepthExceeded).
# Flatten to a standalone qcow2 so the live disk is writable.
if [ "${SKIP_CONVERT:-}" = "1" ] && [ -s "$ROOT" ]; then
  echo "reusing flattened $ROOT"
else
  qemu-img convert -p -O qcow2 "$GOLDEN" "$ROOT"
fi

# cloud-init NoCloud seed (FAT). Prefer cloud-localds; fall back to mkfs.vfat.
USERDATA="$RUN/user-data"
METADATA="$RUN/meta-data"
cat >"$USERDATA" <<'EOF'
#cloud-config
hostname: nauti-jammy
manage_etc_hosts: true
runcmd:
  - mkdir -p /mnt/nauti
  - mount -t virtiofs nauti /mnt/nauti || true
  - bash /mnt/nauti/guest-firstboot.sh
EOF
cat >"$METADATA" <<EOF
instance-id: nauti-m7m8m9-$(date +%s)
local-hostname: nauti-jammy
EOF
if command -v cloud-localds >/dev/null 2>&1; then
  cloud-localds "$SEED" "$USERDATA" "$METADATA"
else
  truncate -s 2M "$SEED"
  mkfs.vfat -n cidata "$SEED"
  mcopy -i "$SEED" "$USERDATA" ::user-data
  mcopy -i "$SEED" "$METADATA" ::meta-data
fi

# Backends
rm -f "$RNG_SOCK" "$FS_SOCK"
"$NAUTI" vhost serve --socket "$RNG_SOCK" --source /dev/urandom >"$RUN/vhost-rng.log" 2>&1 &
echo $! >"$RUN/vhost-rng.pid"
# virtiofsd: --socket-path + --shared-dir (v1.10+); older uses --fd.
if "$VIRTIOFSD" --help 2>&1 | grep -q socket-path; then
  "$VIRTIOFSD" --socket-path "$FS_SOCK" --shared-dir "$STORE" --cache never --sandbox none \
    >"$RUN/virtiofsd.log" 2>&1 &
else
  "$VIRTIOFSD" --socket "$FS_SOCK" --shared-dir "$STORE" --cache never \
    >"$RUN/virtiofsd.log" 2>&1 &
fi
echo $! >"$RUN/virtiofsd.pid"
sleep 1
test -S "$RNG_SOCK"
test -S "$FS_SOCK"

# Argv matches nauti-fabric CloudHypervisorAdapter::build_argv (named
# builtin rng, shared=on, generic-vhost-user, --fs, --vsock). Serial is a
# file so the live proof can be diagnosed without a TTY.
cat >"$RUN/ch.argv.txt" <<EOF
$CH --api-socket path=$API --cpus boot=2 --memory size=2048M,shared=on
  --kernel $IMAGES/vmlinuz-5.15-jammy --initramfs $IMAGES/initrd-5.15-jammy
  --disk path=$ROOT --disk path=$SEED
  --serial file=$SERIAL --console off
  --rng src=/dev/urandom,id=ch-builtin-rng
  --generic-vhost-user device_type=rng,socket=$RNG_SOCK,queue_sizes=1024
  --fs tag=nauti,socket=$FS_SOCK
  --vsock cid=3,socket=$VSOCK_SOCK
  --watchdog
  --cmdline console=ttyS0 root=/dev/vda1 rw modules-load=virtio_rng,virtiofs ds=nocloud
EOF
"$CH" \
  --api-socket "path=$API" \
  --cpus boot=2 \
  --memory size=2048M,shared=on \
  --kernel "$IMAGES/vmlinuz-5.15-jammy" \
  --initramfs "$IMAGES/initrd-5.15-jammy" \
  --disk "path=$ROOT" \
  --disk "path=$SEED" \
  --serial "file=$SERIAL" \
  --console off \
  --rng src=/dev/urandom,id=ch-builtin-rng \
  --generic-vhost-user "device_type=rng,socket=$RNG_SOCK,queue_sizes=1024" \
  --fs "tag=nauti,socket=$FS_SOCK" \
  --vsock "cid=3,socket=$VSOCK_SOCK" \
  --watchdog \
  --cmdline "console=ttyS0 root=/dev/vda1 rw modules-load=virtio_rng,virtiofs ds=nocloud" \
  >"$RUN/ch.stdout" 2>"$RUN/ch.stderr" &
echo $! >"$RUN/ch.pid"
# Also record a nauti-shaped launch line for the fabric CLI (does not
# spawn a second VM; used as the contract reference).
"$NAUTI" vm --help >"$RUN/nauti-vm-help.txt" 2>&1 || true

# CH is detached; confirm Running.
for i in $(seq 1 20); do
  if ch-remote --api-socket="$API" info 2>/dev/null | tee "$RUN/ch-info.json" | grep -q Running; then
    break
  fi
  sleep 1
done

# Wait for guest to pin rng, then kill ONLY the vhost backend.
for i in $(seq 1 90); do
  if [ -f "$STORE/flags/m7-pinned" ]; then
    break
  fi
  sleep 2
done
if [ -f "$STORE/flags/m7-pinned" ]; then
  kill -9 "$(cat "$RUN/vhost-rng.pid")" || true
  sleep 1
  touch "$STORE/flags/kill-done"
fi

# Wait for M8/M9 guest artifacts.
for i in $(seq 1 60); do
  if [ -f "$STORE/flags/m9-done" ]; then
    break
  fi
  sleep 2
done

# Host-side M8: same store.path visible; then Fabric::release detaches via agent.
echo "host-saw-guest $(cat "$STORE/guest-marker.txt" 2>/dev/null || echo missing)" >"$STORE/results/m8-host.txt"
ADDR_FILE="${NAUTI_AGENT_ADDR_DIR:-/var/lib/nauti}/cesarops2-addr.json"
if [ -f "$ADDR_FILE" ]; then
  ADDR=$(cat "$ADDR_FILE")
  if LEASE=$("$NAUTI" fabric lease "$ADDR" --resource-id local.store.fabric --owner m8-live --ttl-secs 60); then
    echo "$LEASE" >"$RUN/m8-lease.json"
    "$NAUTI" fabric attach "$ADDR" --adapter fabric-store --lease-json "$LEASE" | tee "$RUN/m8-attach.json" || true
    "$NAUTI" fabric release "$ADDR" --lease-json "$LEASE" | tee "$RUN/m8-release.txt" || true
  fi
fi

# Host-side M9 PeerJob on this box (guest already wrote its envelope).
python3 - <<'PY' >"$STORE/results/m9-host-observe.json"
import json, os, time
store=os.environ.get("STORE_DIR","/var/lib/nauti/store")
guest=os.path.join(store,"results","m9-peerjob.json")
data={"guest_peerjob_exists": os.path.exists(guest), "ts": time.time()}
if os.path.exists(guest):
    data["guest"]=json.load(open(guest))
print(json.dumps(data, indent=2))
PY

# Record CH still Running after rng kill.
ch-remote --api-socket="$API" info >"$RUN/ch-info-after-kill.json" 2>&1 || true

echo "live artifacts in $RUN and $STORE/results"
ls -la "$STORE/results" "$STORE/flags"
