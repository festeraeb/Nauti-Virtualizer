//! Dataset validation for a LoRA fine-tuning corpus in JSONL form.
//!
//! This is the Rust enforcement of the schema and grounding rules described in
//! the fine-tuning repo's `ADAPTER_CREATION_SPEC.md` §11. It lives in
//! `tools/dataset-validate/` rather than the deployable `nauti-fabric` crate
//! because validating a training corpus is an out-of-band activity: a host
//! running the resource fabric does not need to validate a JSONL, and adding
//! `regex` to the deployable crate's dependency footprint for a feature that
//! nobody exercises at runtime would be unnecessary cost. The binary that
//! wraps this library is at `tools/dataset-validate/src/main.rs` and is built
//! on demand with `cargo run -p dataset-validate -- <path>`.
//!
//! The 11-case negative test that hardened the previous
//! `scripts/validate_dataset.py` implementation (`api_key=`, `bearer_token`,
//! `auth_token`, `password=…`, `secret=…`, `AKIA…` AWS key shape as
//! must-reject; `argv token`, `lexer tokens`, `secret ingredient`,
//! `passwordless auth`, `credentialed vs not` as must-accept) is reproduced as
//! `#[cfg(test)] mod tests` below. The Python script is gone.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

/// One issue found by the validator. The position is the 1-based record index in the
/// JSONL file (i.e. `Record 1` is the first non-empty line). `message` is a single
/// human-readable line; the integration test concatenates them in order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatasetIssue {
    pub record: usize,
    pub message: String,
}

/// The report returned by [`validate_dataset_file`]. `record_count` is the number of
/// non-empty lines that were inspected; `issues` is empty on success.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DatasetReport {
    pub record_count: usize,
    pub issues: Vec<DatasetIssue>,
}

impl DatasetReport {
    pub fn is_ok(&self) -> bool {
        self.issues.is_empty()
    }
}

/// Errors that prevent the validator from even inspecting the file. These are returned
/// from [`validate_dataset_file`], distinct from per-record `DatasetIssue`s.
#[derive(Debug, thiserror::Error)]
pub enum DatasetError {
    #[error("dataset file not found: {0}")]
    NotFound(PathBuf),
    #[error("dataset file could not be read: {0}: {1}")]
    Unreadable(PathBuf, std::io::Error),
}

/// Locate the dataset file relative to a workspace root. Convenience for the
/// integration test; not used internally.
pub fn dataset_path_for_workspace(workspace_root: &Path) -> PathBuf {
    workspace_root.join("training").join("adapter_creator_dataset.jsonl")
}

/// Validate the dataset at `path` and return a [`DatasetReport`]. The file is read
/// once, line by line; non-empty lines are parsed as JSON; each parsed record is
/// checked against the rules in [ADAPTER_CREATION_SPEC.md](../ADAPTER_CREATION_SPEC.md) §11
/// and the curation rules in [DATASET_NOTES.md](../training/DATASET_NOTES.md).
///
/// This function never panics on input data. All malformed records are surfaced as
/// `DatasetIssue`s; the function returns `Ok` unless the file itself is missing or
/// unreadable.
pub fn validate_dataset_file(path: &Path) -> Result<DatasetReport, DatasetError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(DatasetError::NotFound(path.to_path_buf()));
        }
        Err(error) => return Err(DatasetError::Unreadable(path.to_path_buf(), error)),
    };
    Ok(validate_dataset_text(&text))
}

/// Validate a dataset that has already been read into memory. Public so the unit
/// tests below can avoid touching the filesystem, and so a future CLI subcommand
/// can validate from stdin or a string.
pub fn validate_dataset_text(text: &str) -> DatasetReport {
    let mut report = DatasetReport::default();
    for (idx, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let record_number = idx + 1;
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(error) => {
                report.issues.push(DatasetIssue {
                    record: record_number,
                    message: format!("invalid JSON ({error})"),
                });
                continue;
            }
        };
        let object = match value.as_object() {
            Some(object) => object,
            None => {
                report.issues.push(DatasetIssue {
                    record: record_number,
                    message: "top-level value is not a JSON object".to_string(),
                });
                continue;
            }
        };
        report.issues.extend(check_record(record_number, object));
    }
    report.record_count = text.lines().filter(|l| !l.trim().is_empty()).count();
    report
}

// ---------------------------------------------------------------------------
// Per-record check
// ---------------------------------------------------------------------------

const EXPECTED_ROLES: [&str; 3] = ["system", "user", "assistant"];

fn check_record(record: usize, object: &serde_json::Map<String, Value>) -> Vec<DatasetIssue> {
    let mut issues = Vec::new();

    // Rule 2: messages schema (system / user / assistant, in that order, all strings)
    let messages = object.get("messages");
    let messages_array = match messages.and_then(Value::as_array) {
        Some(array) if array.len() == EXPECTED_ROLES.len() => array,
        _ => {
            issues.push(DatasetIssue {
                record,
                message: format!(
                    "expected exactly {} messages, got {}",
                    EXPECTED_ROLES.len(),
                    messages.map(|v| v.to_string()).unwrap_or_else(|| "missing".to_string())
                ),
            });
            return issues; // can't continue without a valid messages array
        }
    };
    for (offset, (message, expected_role)) in messages_array
        .iter()
        .zip(EXPECTED_ROLES.iter().copied())
        .enumerate()
    {
        let actual_role = message.get("role").and_then(Value::as_str);
        if actual_role != Some(expected_role) {
            issues.push(DatasetIssue {
                record,
                message: format!(
                    "message[{offset}]: expected role {expected_role:?}, got {actual_role:?}"
                ),
            });
        }
        let content = message.get("content").and_then(Value::as_str);
        if content.map(str::is_empty).unwrap_or(true) {
            issues.push(DatasetIssue {
                record,
                message: format!("message[{offset}]: empty or non-string content"),
            });
        }
    }
    let assistant_content = messages_array
        .get(2)
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("");

    // Rule 3: grounding
    let grounding = object.get("grounding").and_then(Value::as_str);
    match grounding {
        None => issues.push(DatasetIssue {
            record,
            message: "missing required `grounding` field".to_string(),
        }),
        Some("legacy") => { /* grandfathered */ }
        Some(path) => {
            if path.is_empty() {
                issues.push(DatasetIssue {
                    record,
                    message: "empty `grounding` value".to_string(),
                });
            } else {
                // Reject `..` escapes. We compare resolved paths against the workspace
                // root, which the caller supplies via env! in the integration test.
                // Here we just refuse any `..` segment as a cheap defense.
                if Path::new(path).components().any(|c| matches!(c, std::path::Component::ParentDir)) {
                    issues.push(DatasetIssue {
                        record,
                        message: format!("`grounding` {path:?} escapes the repository root"),
                    });
                } else {
                    // The file must exist; the caller is expected to run the validator
                    // from the workspace root. We compute the path relative to the
                    // current working directory, which `cargo test` sets to the
                    // workspace root for integration tests.
                    let resolved = PathBuf::from(path);
                    if !resolved.exists() {
                        issues.push(DatasetIssue {
                            record,
                            message: format!("`grounding` file {path:?} does not exist"),
                        });
                    } else if resolved.is_file() {
                        // Rule 5 (soft): the answer must share at least one identifier
                        // token with the grounding file. Empty files yield no
                        // intersection; report that.
                        match fs::read_to_string(&resolved) {
                            Ok(file_text) => {
                                let file_tokens = identifier_tokens(&file_text);
                                let content_tokens = identifier_tokens(assistant_content);
                                if file_tokens.is_empty() || content_tokens.is_empty() {
                                    issues.push(DatasetIssue {
                                        record,
                                        message: format!(
                                            "either the grounding file {path:?} or the assistant \
                                             content is empty; the soft grounding check is vacuous"
                                        ),
                                    });
                                } else if file_tokens.is_disjoint(&content_tokens) {
                                    issues.push(DatasetIssue {
                                        record,
                                        message: format!(
                                            "assistant content shares no identifier tokens with \
                                             `{path}`; the answer may be ungrounded"
                                        ),
                                    });
                                }
                            }
                            Err(error) => issues.push(DatasetIssue {
                                record,
                                message: format!(
                                    "cannot read grounding file {path:?}: {error}"
                                ),
                            }),
                        }
                    }
                }
            }
        }
    }

    // Rule 4: forbidden markers in assistant content
    for marker in FORBIDDEN_MARKERS {
        if assistant_content.to_lowercase().contains(&marker.to_lowercase()) {
            issues.push(DatasetIssue {
                record,
                message: format!("assistant content contains forbidden marker {marker:?}"),
            });
        }
    }
    for marker in FORBIDDEN_HOST_MARKERS {
        if assistant_content.contains(marker) {
            issues.push(DatasetIssue {
                record,
                message: format!("assistant content contains forbidden host marker {marker:?}"),
            });
        }
    }
    if SECRET_MARKER_REGEX.is_match(assistant_content) {
        issues.push(DatasetIssue {
            record,
            message: "assistant content references a secret/password/token keyword".to_string(),
        });
    }

    issues
}

// ---------------------------------------------------------------------------
// Static patterns
// ---------------------------------------------------------------------------

const FORBIDDEN_MARKERS: &[&str] = &["TBD", "TODO", "FIXME", "unverified", "placeholder"];

const FORBIDDEN_HOST_MARKERS: &[&str] = &["127.0.0.1", "192.168.", "10.0.0."];

// Credential-shaped patterns only. Bare "token" / "secret" / "password" / "credential"
// as English or code words (e.g. "the second token after --api-socket", "the secret
// ingredient is per-socket state") are not flagged. See the module docstring for the
// test that pins this down.
static SECRET_MARKER_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?xi)
        \b api [_-]? key \b
      | \b (?: auth | access | bearer | refresh | id | client | secret ) [_-]? token \b
      | \b (?: password | passwd | pwd ) \s* [=:]
      | \b (?: secret | credential ) s? \s* [=:]
      | \b AKIA [0-9A-Z]{16} \b
    ",
    )
    .expect("secret-marker regex is well-formed; this is a compile-time invariant")
});

// `regex` doesn't support \b with (?i) well, so we use a simpler form here.
static IDENT_TOKEN_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[a-z][a-z0-9_]{2,}\b").expect("identifier regex is well-formed"));

fn identifier_tokens(text: &str) -> BTreeSet<String> {
    IDENT_TOKEN_REGEX
        .find_iter(text)
        .map(|m| m.as_str().to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(assistant: &str, grounding: Option<&str>) -> String {
        let mut record = serde_json::json!({
            "messages": [
                {"role": "system", "content": "x"},
                {"role": "user", "content": "x"},
                {"role": "assistant", "content": assistant},
            ],
        });
        if let Some(g) = grounding {
            record["grounding"] = serde_json::Value::String(g.to_string());
        }
        record.to_string()
    }

    fn issues_for(assistant: &str, grounding: Option<&str>) -> Vec<DatasetIssue> {
        let line = make_record(assistant, grounding);
        let value: Value = serde_json::from_str(&line).expect("test record is valid JSON");
        check_record(1, value.as_object().unwrap())
    }

    /// Write a temp file with a known set of snake_case identifiers and return its path.
    /// The tempdir is cleaned up on test exit. The validator's soft grounding check
    /// requires the assistant content to share at least one identifier token with the
    /// grounding file; this helper gives the tests a hermetic grounding target.
    fn temp_grounding_with_identifiers(identifiers: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dataset-validate-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create tempdir");
        let path = dir.join("grounding.rs");
        let body = identifiers
            .iter()
            .map(|id| format!("fn {id}() {{ }}\n"))
            .collect::<String>();
        fs::write(&path, body).expect("write grounding file");
        path
    }

    #[test]
    fn clean_record_with_grounding_in_known_file_passes() {
        // Rust code is snake_case; the validator's identifier regex matches
        // snake_case, so the test grounding file uses snake_case identifiers
        // and the assistant content references one of them.
        let path = temp_grounding_with_identifiers(&["fabric_error", "resource_kind"]);
        let issues = issues_for(
            "this mentions fabric_error by name",
            Some(path.to_str().unwrap()),
        );
        assert!(issues.is_empty(), "expected no issues, got {issues:?}");
    }

    #[test]
    fn legacy_grounding_skips_existence_and_overlap_checks() {
        let issues = issues_for("anything", Some("legacy"));
        assert!(issues.is_empty(), "legacy should be grandfathered; got {issues:?}");
    }

    #[test]
    fn missing_grounding_is_reported() {
        let issues = issues_for("hello", None);
        assert!(issues.iter().any(|i| i.message.contains("missing required `grounding`")));
    }

    #[test]
    fn grounding_pointing_at_a_missing_file_is_reported() {
        let issues = issues_for("hello", Some("nauti-fabric/src/this_does_not_exist.rs"));
        assert!(issues.iter().any(|i| i.message.contains("does not exist")));
    }

    #[test]
    fn grounding_with_dotdot_is_rejected_even_if_a_real_file_exists() {
        let issues = issues_for("hello", Some("../etc/passwd"));
        assert!(issues.iter().any(|i| i.message.contains("escapes the repository root")));
    }

    #[test]
    fn assistant_with_tbd_marker_is_reported() {
        let issues = issues_for("TBD answer", Some("legacy"));
        assert!(issues.iter().any(|i| i.message.contains("forbidden marker \"TBD\"")));
    }

    #[test]
    fn assistant_with_host_marker_is_reported() {
        let issues = issues_for("connect to 192.168.1.5", Some("legacy"));
        assert!(issues.iter().any(|i| i.message.contains("host marker")));
    }

    #[test]
    fn assistant_with_credential_shape_is_reported() {
        // Each of these must trigger the secret marker; each is one of the
        // shapes the Python validator caught after the tightening.
        let must_reject = [
            "the value of api_key is set",                  // api_key=
            "use the bearer_token from the env",            // bearer_token
            "pass the auth_token in the header",            // auth_token
            "password=hunter2 is bad",                      // password=
            "secret=topsecret is bad",                      // secret=
            "the AKIAIOSFODNN7EXAMPLE leaked",              // AWS key
        ];
        for content in must_reject {
            let issues = issues_for(content, Some("legacy"));
            assert!(
                issues.iter().any(|i| i.message.contains("secret/password/token")),
                "expected secret-shape rejection for {content:?}, got {issues:?}"
            );
        }
    }

    #[test]
    fn english_uses_of_token_secret_password_are_not_reported() {
        // Each of these is a legitimate English/code use; none should fire.
        let must_accept = [
            "the second token after --api-socket is the socket path",
            "extract identifier tokens from the source file",
            "the secret ingredient is the per-socket state map",
            "passwordless auth is the right default for this adapter",
            "the adapter is credentialed to its own state, not the host's",
        ];
        for content in must_accept {
            let issues = issues_for(content, Some("legacy"));
            assert!(
                !issues.iter().any(|i| i.message.contains("secret/password/token")),
                "false positive on legitimate use: {content:?}; got {issues:?}"
            );
        }
    }

    #[test]
    fn assistant_with_no_identifier_overlap_with_grounding_is_soft_reported() {
        // Use a temp grounding file with one set of identifiers; the assistant
        // content uses a different set, so the soft check fires.
        let path = temp_grounding_with_identifiers(&["alpha_one", "beta_two", "gamma_three"]);
        let issues = issues_for(
            "zqzqzqzqz qzqzqzqz completely unrelated gibberish",
            Some(path.to_str().unwrap()),
        );
        assert!(
            issues.iter().any(|i| i.message.contains("shares no identifier tokens")),
            "expected soft grounding rejection, got {issues:?}"
        );
    }

    #[test]
    fn wrong_role_order_is_reported() {
        let line = serde_json::json!({
            "messages": [
                {"role": "user", "content": "x"},
                {"role": "system", "content": "x"},
                {"role": "assistant", "content": "ok."},
            ],
            "grounding": "nauti-fabric/src/lib.rs",
        })
        .to_string();
        let value: Value = serde_json::from_str(&line).unwrap();
        let issues = check_record(1, value.as_object().unwrap());
        assert!(issues.iter().any(|i| i.message.contains("expected role \"system\"")));
        assert!(issues.iter().any(|i| i.message.contains("expected role \"user\"")));
    }

    #[test]
    fn validate_dataset_text_reports_invalid_json_and_non_object_top_level() {
        let report = validate_dataset_text(
            "{\"messages\":[\"a\", \"b\", \"c\"]}\nnot json at all\n\"just a string\"\n",
        );
        // First line is valid JSON but not an object. The validator reports each issue.
        // We expect at least: top-level not object, invalid JSON, top-level not object.
        assert!(report.issues.iter().any(|i| i.message.contains("not a JSON object")));
        assert!(report.issues.iter().any(|i| i.message.contains("invalid JSON")));
        // record_count counts the non-empty lines.
        assert_eq!(report.record_count, 3);
    }
}
