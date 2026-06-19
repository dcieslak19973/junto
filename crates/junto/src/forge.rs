//! Opening pull requests on a git forge — the code-PR Playbook's outward
//! action (`docs/adr/0027` lineage is unrelated; this is the push-gate's
//! deliverable, deferred in ledger `ba64074b`).
//!
//! Vendor-quarantined by design (constraint #4): the kernel never names a
//! forge. GitHub is driven by the **`gh` CLI** — it inherits the user's
//! existing GitHub auth (credential managers, SSH, enterprise SSO) the same
//! way the substrate shells out to `git`, so junto custodies no forge tokens.
//! A `ForgeAdapter` trait extracts when a second forge (GitLab/Bitbucket)
//! lands — rule of three; one concrete impl does not earn the abstraction yet
//! (cf. `docs/adr/0024`, which obviated the harness trait the same way).

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

/// What to open: a head branch (already pushed to the remote) merged against a
/// base, with a title and body. Repo-relative so `gh` infers the remote.
#[derive(Debug, Clone)]
pub struct PullRequestSpec {
    /// The local repo whose remote the PR is opened on (the op runs here).
    pub repo: PathBuf,
    /// The branch carrying the changes (already pushed).
    pub head: String,
    /// The branch to merge into, e.g. `main`.
    pub base: String,
    /// The PR title.
    pub title: String,
    /// The PR body (markdown).
    pub body: String,
}

/// A GitHub forge driven by the `gh` CLI.
pub struct GithubForge;

impl GithubForge {
    /// The argv for `gh pr create` (pure, so it is unit-testable without a
    /// remote). `gh` infers the repo/remote from the working directory and
    /// prints the new PR's URL to stdout.
    fn pr_create_args(spec: &PullRequestSpec) -> Vec<String> {
        vec![
            "pr".into(),
            "create".into(),
            "--head".into(),
            spec.head.clone(),
            "--base".into(),
            spec.base.clone(),
            "--title".into(),
            spec.title.clone(),
            "--body".into(),
            spec.body.clone(),
        ]
    }

    /// Open the pull request, returning its URL. Runs `gh pr create` in the
    /// repo (so `gh` resolves the remote); the branch must already be pushed.
    /// No shell — argv is passed directly, so titles/bodies need no escaping.
    pub fn open_pull_request(&self, spec: &PullRequestSpec) -> Result<String> {
        let mut command = std::process::Command::new("gh");
        command
            .args(Self::pr_create_args(spec))
            .current_dir(&spec.repo);
        // Terminal-less: no flashed console window (constraint #2).
        crate::launch::no_console_window(&mut command);
        let output = command
            .output()
            .context("running `gh pr create` (is the GitHub CLI installed and on PATH?)")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("`gh pr create` failed: {}", stderr.trim());
        }
        let url = String::from_utf8(output.stdout)
            .context("gh output is not utf-8")?
            .trim()
            .to_string();
        if url.is_empty() {
            bail!("`gh pr create` succeeded but printed no PR url");
        }
        Ok(url)
    }

    /// Whether this forge can open PRs here: `gh` is installed and
    /// authenticated (`gh auth status` exits 0). The capability probe
    /// (constraint #4: branch on capability, not vendor identity) — checked
    /// before proposing a PR-open so the gate is never offered when it can't be
    /// honored.
    pub fn is_available() -> bool {
        let mut command = std::process::Command::new("gh");
        command.args(["auth", "status"]);
        crate::launch::no_console_window(&mut command);
        command
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> PullRequestSpec {
        PullRequestSpec {
            repo: PathBuf::from("/r"),
            head: "junto/abc123".into(),
            base: "main".into(),
            title: "Add the thing".into(),
            body: "It does the thing.\n\nVerified green.".into(),
        }
    }

    #[test]
    fn pr_create_args_carry_head_base_title_body() {
        assert_eq!(
            GithubForge::pr_create_args(&spec()),
            vec![
                "pr",
                "create",
                "--head",
                "junto/abc123",
                "--base",
                "main",
                "--title",
                "Add the thing",
                "--body",
                "It does the thing.\n\nVerified green.",
            ]
        );
    }
}
