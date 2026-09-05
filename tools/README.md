# `tools/` — Optional, Out-of-Band Utilities

This directory holds **auxiliary tools** that are useful to people working on
Nauti Virtualizer but are **not part of the deployable crate**. They live in
their own workspace members so they:

- Are not built by `cargo build` in the deployable crate.
- Are not run by `cargo test --workspace` against the deployable crate.
- Have their own dependency footprint (e.g. `regex` for the dataset validator),
  which the deployable crate never pulls in.
- Can be versioned, tested, and evolved independently of the deployable.

Each tool is a small Rust binary you build on demand with
`cargo run -p <name> -- <args>`.

## `dataset-validate` — LoRA corpus validator

Validates a JSONL file of chat records against the v0.1.0 schema and grounding
rules described in the fine-tuning repo's `ADAPTER_CREATION_SPEC.md` §11.
Useful when you are curating your own fine-tuning corpus in the same shape
as the seed corpus.

This binary is the Rust rewrite of the earlier `scripts/validate_dataset.py`
Python shim. The behavior is identical: it checks the chat schema, the
`grounding` field, forbidden markers, and the soft identifier-overlap rule.
The 11-case negative test (credential shapes vs. English/code uses of
`token`/`secret`/`password`/`credential`) is reproduced as
`dataset_validate::tests`.

### Build and run

```bash
# from the workspace root
cargo run -p dataset-validate --release -- /path/to/your-corpus.jsonl
```

The binary exits 0 on success, 1 on validation failure, 2 on usage error
(missing path, unreadable file). Diagnostic lines are written to stderr.

### Why is it not in `nauti-fabric/`?

The deployable crate (`nauti-fabric`) is what ships to a host that runs
the resource fabric. A host running the fabric does not need to validate a
LoRA corpus, and adding `regex` to the deployable crate's dependency
footprint for a feature that nobody exercises at runtime would be
unnecessary cost. The tool lives here so the validator's logic is still
discoverable, buildable, and usable by anyone who is curating data, while
keeping it out of the lib.

### Adding a new tool

Drop a new directory under `tools/`, give it a `Cargo.toml` with the binary
target, and add it to the root `Cargo.toml` `[workspace] members = [...]`
list. Keep the dependency footprint minimal and document the build/run
instructions in this file.
