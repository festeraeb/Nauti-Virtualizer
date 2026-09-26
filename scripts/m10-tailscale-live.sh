#!/bin/bash
# M10 live: c3 submits a wgpu add job to c2 over Tailscale only.
# LAN 192.168.4.x is refused by the binary.
set -euo pipefail
C2_TS="${C2_TS:-100.109.55.22}"
C3_TS="${C3_TS:-100.76.255.40}"
PORT="${NAUTI_WGPU_PORT:-9110}"
NAUTI="${NAUTI_BIN:-/usr/local/bin/nauti}"

echo "=== research bucket ==="
"$NAUTI" research discover --json | tee /var/lib/nauti/research-memory.json

case "$(hostname)" in
  cesarops2)
    sudo ufw allow from 100.64.0.0/10 to any port "$PORT" proto tcp comment nauti-wgpu || true
    exec "$NAUTI" wgpu serve --bind "${C2_TS}:${PORT}"
    ;;
  cesarops3)
    "$NAUTI" wgpu run --connect "${C2_TS}:${PORT}" --n 4096 | tee /var/lib/nauti/m10-wgpu-result.json
    ;;
  *)
    echo "run on cesarops2 (serve) or cesarops3 (run)" >&2
    exit 1
    ;;
esac
