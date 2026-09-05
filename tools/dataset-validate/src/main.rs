//! `dataset-validate` — optional tool for curating a LoRA fine-tuning corpus.
//!
//! This binary is **not** part of the deployable project. It lives under
//! `tools/` and is built on demand with
//! `cargo run -p dataset-validate -- <path-to-corpus.jsonl>`. It validates a
//! JSONL of chat records against the schema and grounding rules documented in
//! `ADAPTER_CREATION_SPEC.md` §11 (which is the prompt template used to
//! teach a base model the adapter-creation contract).
//!
//! Use it when curating your own corpus. The deployable project's
//! `cargo test --workspace` never invokes this binary, and the deployable
//! crate has no runtime or test-time dependency on it.

use std::path::PathBuf;
use std::process::ExitCode;

use dataset_validate::{validate_dataset_file, DatasetError};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 1 {
        eprintln!(
            "usage: dataset-validate <path-to-corpus.jsonl>\n\n\
             Validates a JSONL of chat records against the v0.1.0 schema and\n\
             grounding rules. See ADAPTER_CREATION_SPEC.md §11 in the fine-\n\
             tuning repo for the full contract."
        );
        return ExitCode::from(2);
    }

    let path = PathBuf::from(&args[0]);
    let report = match validate_dataset_file(&path) {
        Ok(report) => report,
        Err(DatasetError::NotFound(p)) => {
            eprintln!("error: corpus file not found: {}", p.display());
            return ExitCode::from(2);
        }
        Err(DatasetError::Unreadable(p, error)) => {
            eprintln!(
                "error: corpus file at {} could not be read: {error}",
                p.display()
            );
            return ExitCode::from(2);
        }
    };

    println!(
        "validated {} record(s) in {}",
        report.record_count,
        path.display()
    );

    if report.is_ok() {
        println!("OK: all records valid");
        ExitCode::SUCCESS
    } else {
        eprintln!("FAIL: {} issue(s)", report.issues.len());
        for issue in &report.issues {
            eprintln!("  - record {}: {}", issue.record, issue.message);
        }
        ExitCode::from(1)
    }
}
