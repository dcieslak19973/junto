//! The mechanical verify pipeline for the code-PR Playbook (`docs/adr/0025`'s
//! Outcome + Rubric model — this is the *mechanical* half; the Grader scores the
//! Rubric). It runs an ordered list of check commands in a workspace and reports
//! each one's pass/fail plus captured output, which the Outcome loop turns into
//! findings (fed back to the worker) and Artifacts (the durable record).
//!
//! Checks run as **program + args, no shell** — the commands are simple tool
//! invocations (`cargo fmt --check`), so direct spawning sidesteps the
//! PowerShell-vs-sh split (CLAUDE.md cross-platform rules).

use std::path::Path;
use std::process::Command;

/// One mechanical check to run in a workspace (e.g. `cargo fmt --check`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Check {
    /// Short label for the finding (e.g. `fmt`).
    pub(crate) name: String,
    /// The command line, run as program + args (no shell).
    pub(crate) command: String,
}

/// The result of running one [`Check`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CheckResult {
    /// The check's name.
    pub(crate) name: String,
    /// Whether the command exited successfully (status 0).
    pub(crate) passed: bool,
    /// Combined stdout + stderr — for the finding fed back to the worker and the
    /// captured Artifact.
    pub(crate) output: String,
}

/// The default code-PR mechanical checks for a cargo workspace — CLAUDE.md's
/// "is it green?" order (`fmt --check` → `clippy -D warnings` → `test`). v1 uses
/// this for every workspace; a per-repo `~/.junto/verify.toml` override slots in
/// here when a second repo needs different checks (rule of three).
pub(crate) fn default_cargo_checks() -> Vec<Check> {
    [
        ("fmt", "cargo fmt --check"),
        (
            "clippy",
            "cargo clippy --workspace --all-targets -- -D warnings",
        ),
        ("test", "cargo test --workspace"),
    ]
    .into_iter()
    .map(|(name, command)| Check {
        name: name.to_string(),
        command: command.to_string(),
    })
    .collect()
}

/// Run each check in `workspace`, in order, returning one result per check.
pub(crate) fn run_checks(workspace: &Path, checks: &[Check]) -> Vec<CheckResult> {
    checks
        .iter()
        .map(|check| run_one(workspace, check))
        .collect()
}

/// Build the worker-feedback block for any failed checks, or `None` when every
/// check passed (so the Outcome loop proceeds to the Grader). Only failed checks
/// appear, each with its captured output, so the worker knows what to fix.
pub(crate) fn mechanical_feedback(results: &[CheckResult]) -> Option<String> {
    let failed: Vec<&CheckResult> = results.iter().filter(|r| !r.passed).collect();
    if failed.is_empty() {
        return None;
    }
    let mut block = String::from("The following mechanical checks failed:\n");
    for result in failed {
        block.push_str(&format!("\n## {}\n{}\n", result.name, result.output.trim()));
    }
    Some(block)
}

/// Run a single check as program + args (no shell), capturing combined output.
/// A spawn failure (e.g. the tool isn't installed) is itself a failed check.
fn run_one(workspace: &Path, check: &Check) -> CheckResult {
    let mut parts = check.command.split_whitespace();
    let Some(program) = parts.next() else {
        return CheckResult {
            name: check.name.clone(),
            passed: false,
            output: "empty command".to_string(),
        };
    };
    let args: Vec<&str> = parts.collect();
    match Command::new(program)
        .args(&args)
        .current_dir(workspace)
        .output()
    {
        Ok(out) => {
            let mut output = String::from_utf8_lossy(&out.stdout).into_owned();
            output.push_str(&String::from_utf8_lossy(&out.stderr));
            CheckResult {
                name: check.name.clone(),
                passed: out.status.success(),
                output,
            }
        }
        Err(err) => CheckResult {
            name: check.name.clone(),
            passed: false,
            output: format!("failed to run `{}`: {err}", check.command),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_passing_check() {
        let ws = tempfile::tempdir().expect("tempdir");
        let checks = vec![Check {
            name: "version".to_string(),
            command: "cargo --version".to_string(),
        }];
        let results = run_checks(ws.path(), &checks);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "version");
        assert!(results[0].passed, "cargo --version should exit 0");
        assert!(
            !results[0].output.is_empty(),
            "should capture cargo's version output"
        );
    }

    #[test]
    fn reports_a_failing_check() {
        // `cargo build` in an empty dir fails (no Cargo.toml) — a deterministic,
        // cross-platform non-zero exit.
        let ws = tempfile::tempdir().expect("tempdir");
        let checks = vec![Check {
            name: "build".to_string(),
            command: "cargo build".to_string(),
        }];
        let results = run_checks(ws.path(), &checks);
        assert_eq!(results.len(), 1);
        assert!(
            !results[0].passed,
            "cargo build with no manifest should fail"
        );
        assert!(
            !results[0].output.is_empty(),
            "should capture cargo's error output"
        );
    }

    #[test]
    fn missing_tool_is_a_failed_check() {
        let ws = tempfile::tempdir().expect("tempdir");
        let checks = vec![Check {
            name: "ghost".to_string(),
            command: "junto-no-such-tool-xyz --version".to_string(),
        }];
        let results = run_checks(ws.path(), &checks);
        assert_eq!(results.len(), 1);
        assert!(!results[0].passed, "a missing tool is a failed check");
    }

    #[test]
    fn runs_checks_in_order() {
        let ws = tempfile::tempdir().expect("tempdir");
        let checks = vec![
            Check {
                name: "first".to_string(),
                command: "cargo --version".to_string(),
            },
            Check {
                name: "second".to_string(),
                command: "cargo --version".to_string(),
            },
        ];
        let names: Vec<_> = run_checks(ws.path(), &checks)
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names, vec!["first", "second"]);
    }

    fn result(name: &str, passed: bool, output: &str) -> CheckResult {
        CheckResult {
            name: name.to_string(),
            passed,
            output: output.to_string(),
        }
    }

    #[test]
    fn no_feedback_when_all_checks_pass() {
        let results = vec![result("fmt", true, ""), result("test", true, "ok")];
        assert_eq!(mechanical_feedback(&results), None);
    }

    #[test]
    fn feedback_lists_only_failed_checks_with_output() {
        let results = vec![
            result("fmt", true, "fine"),
            result("clippy", false, "error: unused variable `x`"),
        ];
        let feedback = mechanical_feedback(&results).expect("some checks failed");
        assert!(feedback.contains("clippy"), "names the failed check");
        assert!(
            feedback.contains("unused variable"),
            "includes the failed check's output"
        );
        assert!(!feedback.contains("fmt"), "does not list passing checks");
    }

    #[test]
    fn default_checks_are_the_cargo_green_sequence() {
        let checks = default_cargo_checks();
        let names: Vec<_> = checks.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["fmt", "clippy", "test"]);
        // The commands match CLAUDE.md's "is it green?" order.
        assert!(checks[0].command.contains("fmt"));
        assert!(checks[1].command.contains("clippy"));
        assert!(checks[2].command.contains("test"));
    }
}
