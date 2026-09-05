#!/usr/bin/env python3
"""Validate training/adapter_creator_dataset.jsonl against the v0.1.0 spec.

Enforced rules (per ADAPTER_CREATION_SPEC.md §11 and the existing
training/DATASET_NOTES.md curation rules):

  1. Every line is valid JSON.
  2. Every record has a `messages` array with exactly three entries,
     roles system / user / assistant, in that order.
  3. Every record has a top-level `grounding` field.
     - The legacy value `"legacy"` is allowed for pre-spec records.
     - Any other value must be a relative path to a file that exists
       inside the repository root (this directory's parent of the
       `training/` directory).
  4. The `assistant` content must not contain:
     - placeholder markers: "TBD", "TODO", "FIXME", "unverified"
     - private host markers: "127.0.0.1", "192.168.", "10.0.0.",
     - the literal phrase "secret" or "password" (case-insensitive),
       since real secrets are forbidden by DATASET_NOTES.md.
  5. Records added after this script (i.e. not carrying
     `grounding: legacy`) must have a non-empty `assistant` content
     that references a function/type name present in the grounding
     file (a soft check: at least one identifier-shaped token from
     the file appears in the answer).

Exits 0 on success, 1 on any failure, with one diagnostic per failure.
Designed to be run from CI: `python3 scripts/validate_dataset.py`.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

REPO_ROOT = Path(__file__).resolve().parent.parent
DATASET = REPO_ROOT / "training" / "adapter_creator_dataset.jsonl"

# Markers the curator must never leave behind in a finished record.
FORBIDDEN_MARKERS = [
    "TBD",
    "TODO",
    "FIXME",
    "unverified",
    "placeholder",  # "this is a placeholder" sneaks past TODO
]

# Host/IP-like patterns that should never appear in shipped data.
FORBIDDEN_HOST_MARKERS = [
    "127.0.0.1",
    "192.168.",
    "10.0.0.",
]

# Catches accidental copy-paste of "secret"/"password" content.
# Security-meaningful only. Bare words like “token” (argv token, lexer token) or
# “secret” (a secret ingredient) are not flagged; only the credential-shaped forms.
SECRET_MARKER_PATTERNS = [
    re.compile(r"\bapi[_-]?key\b", re.IGNORECASE),
    re.compile(r"\b(auth|access|bearer|refresh|id|client|secret)[_-]?token\b", re.IGNORECASE),
    re.compile(r"\b(password|passwd|pwd)\s*[=:]", re.IGNORECASE),
    re.compile(r"\b(secret|credential)s?\s*[=:]", re.IGNORECASE),
    re.compile(r"\bAKIA[0-9A-Z]{16}\b"),  # AWS access key id shape
]

# Identifier-shaped token; used by the soft grounding-presence check.
IDENT_RE = re.compile(r"\b[a-z][a-z0-9_]{2,}\b")

# Roles and order required by the chat schema.
EXPECTED_ROLES = ["system", "user", "assistant"]


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def identifier_tokens(text: str) -> set[str]:
    """Return the set of snake_case identifier-shaped tokens in `text`."""
    return {m.group(0) for m in IDENT_RE.finditer(text)}


def check_record(idx: int, rec: dict) -> list[str]:
    errors: list[str] = []

    # Rule 2: messages schema
    messages = rec.get("messages")
    if not isinstance(messages, list) or len(messages) != 3:
        errors.append(f"record {idx}: expected exactly 3 messages, got {messages!r}")
        return errors  # can't continue
    for offset, (expected, msg) in enumerate(zip(EXPECTED_ROLES, messages)):
        if not isinstance(msg, dict):
            errors.append(f"record {idx} message[{offset}]: not an object")
            continue
        if msg.get("role") != expected:
            errors.append(
                f"record {idx} message[{offset}]: expected role {expected!r}, got {msg.get('role')!r}"
            )
        if not isinstance(msg.get("content"), str) or not msg["content"].strip():
            errors.append(f"record {idx} message[{offset}]: empty or non-string content")

    assistant_content = messages[2].get("content", "")

    # Rule 3: grounding
    grounding = rec.get("grounding")
    if grounding is None:
        errors.append(f"record {idx}: missing required `grounding` field")
    elif grounding != "legacy":
        # Must be a relative path that exists.
        if not isinstance(grounding, str) or not grounding:
            errors.append(f"record {idx}: invalid `grounding` value {grounding!r}")
        else:
            target = (REPO_ROOT / grounding).resolve()
            # Defence against ../ escapes.
            try:
                target.relative_to(REPO_ROOT.resolve())
            except ValueError:
                errors.append(
                    f"record {idx}: `grounding` {grounding!r} escapes the repository root"
                )
                target = None
            if target is not None and not target.exists():
                errors.append(
                    f"record {idx}: `grounding` file {grounding!r} does not exist"
                )

    # Rule 4: forbidden content
    for marker in FORBIDDEN_MARKERS:
        if marker.lower() in assistant_content.lower():
            errors.append(
                f"record {idx}: assistant content contains forbidden marker {marker!r}"
            )
    for marker in FORBIDDEN_HOST_MARKERS:
        if marker in assistant_content:
            errors.append(
                f"record {idx}: assistant content contains forbidden host marker {marker!r}"
            )
    if any(pattern.search(assistant_content) for pattern in SECRET_MARKER_PATTERNS):
        errors.append(
            f"record {idx}: assistant content references a secret/password/token keyword"
        )

    # Rule 5: soft grounding-presence check (only for non-legacy records).
    if grounding and grounding != "legacy" and isinstance(grounding, str):
        target = REPO_ROOT / grounding
        if target.exists() and target.is_file():
            try:
                file_text = target.read_text(encoding="utf-8", errors="replace")
            except OSError as exc:
                errors.append(f"record {idx}: cannot read grounding file {grounding!r}: {exc}")
            else:
                file_tokens = identifier_tokens(file_text)
                content_tokens = identifier_tokens(assistant_content)
                # Look for *content-only* tokens that also appear in the file.
                overlap = content_tokens & file_tokens
                if not overlap:
                    errors.append(
                        f"record {idx}: assistant content shares no identifier tokens with "
                        f"`{grounding}`; the answer may be ungrounded"
                    )

    return errors


def main() -> int:
    if not DATASET.exists():
        print(f"dataset not found: {DATASET}", file=sys.stderr)
        return 1

    with DATASET.open() as f:
        raw = f.read()

    # JSONL: split on newlines, drop empty fragments.
    lines = [ln for ln in raw.split("\n") if ln.strip()]
    print(f"validating {len(lines)} record(s) in {DATASET.relative_to(REPO_ROOT)}")

    all_errors: list[str] = []
    for idx, line in enumerate(lines, start=1):
        try:
            rec = json.loads(line)
        except json.JSONDecodeError as exc:
            all_errors.append(f"record {idx}: invalid JSON ({exc})")
            continue
        if not isinstance(rec, dict):
            all_errors.append(f"record {idx}: top-level value is not an object")
            continue
        all_errors.extend(check_record(idx, rec))

    if all_errors:
        print(f"FAIL: {len(all_errors)} error(s)", file=sys.stderr)
        for err in all_errors:
            print(f"  - {err}", file=sys.stderr)
        return 1

    print("OK: all records valid")
    return 0


if __name__ == "__main__":
    sys.exit(main())
