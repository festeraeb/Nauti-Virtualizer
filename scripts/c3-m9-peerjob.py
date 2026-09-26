#!/usr/bin/env python3
"""Same PeerJob envelope on cesarops3 bare metal. Kill-the-VM must not rewrite this."""
from __future__ import annotations

import json
import os
import socket
import time
from pathlib import Path

OUT = Path(os.environ.get("NAUTI_STATE_DIR", "/var/lib/nauti")) / "m9-bare-peerjob.json"


def run_peer_job(job: dict, output: dict) -> dict:
    return {
        "job_id": job["job_id"],
        "domain": job["domain"],
        "status": "ok",
        "output": output,
    }


def main() -> int:
    job = {
        "job_id": "m9-live-1",
        "domain": {
            "node": socket.gethostname(),
            "kind": "bare",
            "endpoint": "local",
        },
        "op": "echo",
        "store_input": "local.store.fabric",
        "payload": {"store.input": "local.store.fabric", "n": 1},
    }
    result = run_peer_job(job, {"n": 1, "alive": True, "source": "bare"})
    payload = {"job": job, "result": result, "ts": time.time()}
    OUT.parent.mkdir(parents=True, exist_ok=True)
    tmp = OUT.with_suffix(".tmp")
    tmp.write_text(json.dumps(payload, indent=2) + "\n")
    tmp.replace(OUT)
    print(json.dumps(payload, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
