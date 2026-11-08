#!/bin/bash
# End-to-end check of the trust registry on one host: a real agent in
# `virtualizer` mode must refuse an identity that is not enrolled, serve it
# once enrolled, and refuse it again after it is ignored (live reload, no restart).
#
#   scripts/trust_e2e.sh [path/to/nauti]
#
# Uses a throwaway directory; touches nothing under /var/lib/nauti or ~/.config.
set -u
NAUTI=${1:-${CARGO_TARGET_DIR:-target}/debug/nauti}
T=$(mktemp -d /tmp/nauti-trust-e2e.XXXXXX)
export NAUTI_AGENT_ADDR_DIR=$T/agent NAUTI_REGISTRY=$T/registry.json
mkdir -p "$T/agent"
fail=0
check() { if [ "$2" = "$3" ]; then echo "PASS $1"; else echo "FAIL $1 (wanted $3, got $2)"; fail=1; fi; }
cleanup() { [ -n "${PID:-}" ] && kill "$PID" 2>/dev/null; sleep 1; rm -rf "$T"; }
trap cleanup EXIT

"$NAUTI" trust set-provider virtualizer >/dev/null
setsid "$NAUTI" agent-serve --node e2e --trust virtualizer > "$T/serve.log" 2>&1 < /dev/null &
PID=$!
for i in $(seq 1 60); do grep -q '^{' "$T/serve.log" 2>/dev/null && break; sleep 1; done
ADDR=$(grep -m1 '^{' "$T/serve.log")
[ -z "$ADDR" ] && { echo "FAIL agent did not start"; cat "$T/serve.log"; exit 1; }

PUB=$("$NAUTI" trust whoami --key-file "$T/client.key" | sed -n 's/^pubkey.*: //p')
NID=$("$NAUTI" trust whoami --key-file "$T/client.key" | sed -n 's/^node_id: //p')

timeout 60 "$NAUTI" agent-connect "$ADDR" --key-file "$T/client.key" > "$T/c1.out" 2>&1
[ -n "${VERBOSE:-}" ] && { echo "--- unenrolled client saw:"; head -4 "$T/c1.out" | cut -c1-200; }
grep -q "not enrolled" "$T/c1.out"; check "unenrolled client is refused" "$?" "0"

"$NAUTI" trust enroll "$PUB" --label e2e-client --url http://127.0.0.1:1 >/dev/null
timeout 60 "$NAUTI" agent-connect "$ADDR" --key-file "$T/client.key" > "$T/c2.out" 2>&1
grep -q "ping -> Pong" "$T/c2.out"; check "accepted client is served" "$?" "0"

sleep 1.2   # mtime granularity
"$NAUTI" trust ignore "$NID" >/dev/null
timeout 60 "$NAUTI" agent-connect "$ADDR" --key-file "$T/client.key" > "$T/c3.out" 2>&1
[ -n "${VERBOSE:-}" ] && { echo "--- ignored client saw:"; head -4 "$T/c3.out" | cut -c1-200; }
grep -q "ping -> Pong" "$T/c3.out"; check "ignored client is refused again without a restart" "$?" "1"

exit $fail
