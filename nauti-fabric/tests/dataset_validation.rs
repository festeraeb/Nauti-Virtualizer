//! Integration test that runs `scripts/validate_dataset.py` against
//! `training/adapter_creator_dataset.jsonl` and fails the build if the dataset
//! is invalid.
//!
//! This is the machine-checkable enforcement arm of
//! `ADAPTER_CREATION_SPEC.md` §11: every dataset record must carry a valid
//! `grounding` field, every assistant answer must avoid forbidden markers,
//! and the chat schema must be `system / user / assistant` in that order.
//!
//! The test shells out to `python3`; if `python3` is missing on the host,
//! the test fails with a clear message rather than passing silently, so the
//! missing-prerequisite is loud in CI.

use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at the crate root (nauti-fabric/). The
    // dataset and validator live in the workspace root, one level up.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("nauti-fabric has a parent workspace root")
        .to_path_buf()
}

#[test]
fn training_dataset_passes_spec_v0_1_0_validation() {
    let workspace = workspace_root();
    let validator = workspace.join("scripts").join("validate_dataset.py");
    assert!(
        validator.exists(),
        "validator script not found at {} — cannot enforce ADAPTER_CREATION_SPEC.md §11",
        validator.display()
    );

    let python = which_python3().unwrap_or_else(|| {
        panic!(
            "`python3` is required on PATH to run the dataset validator ({})… \
             install it or skip this test by removing tests/dataset_validation.rs",
            validator.display()
        )
    });

    let output = Command::new(&python)
        .arg(&validator)
        .current_dir(&workspace)
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "failed to spawn `{} {}`: {error}",
                python.display(),
                validator.display()
            )
        });

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "dataset validator failed (exit {:?}):\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status.code(),
        stdout,
        stderr
    );

    // Defensive: the validator must have actually loaded at least one record.
    // (An empty dataset would also pass; we want a loud failure if someone
    // truncates the file.)
    assert!(
        stdout.contains("validating ") && stdout.contains("record(s)"),
        "validator stdout did not contain a record count; got: {stdout}"
    );
}

fn which_python3() -> Option<PathBuf> {
    // Prefer `python3` on PATH; allow `python` as a fallback for hosts that
    // symlink only the unversioned name (common on Windows CI).
    for candidate in ["python3", "python"] {
        if let Ok(path) = which(candidate) {
            return Some(path);
        }
    }
    None
}

fn which(program: &str) -> Result<PathBuf, ()> {
    let path_var = std::env::var_os("PATH").ok_or(())?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(())
}
