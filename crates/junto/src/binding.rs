//! Channel binding — which channel(s) a working checkout consults and records
//! into (`docs/domain-model.md` "Channel binding").
//!
//! A binding is a property of the **working checkout**, never derivable from
//! the repo (channels are repo-agnostic, `docs/adr/0014`; worktrees of one
//! repo pursue different inquiries):
//!
//! - `.junto.toml` (committed) — the project's ambient channel(s); every
//!   clone/worktree inherits it.
//! - `.junto.local.toml` (gitignored) — this checkout's additional channels,
//!   e.g. the inquiry a worktree exists for. Because it is gitignored, a fresh
//!   `git worktree add` never carries it over; [`seed_member_code_from`]
//!   auto-heals that by copying the agent code from the primary worktree.
//!
//! The effective binding is the union, committed first. This is the
//! dogfood-era bridge: the destination is membership (join at session start,
//! `docs/adr/0013`).

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// File name of the committed project binding.
pub const PROJECT_BINDING: &str = ".junto.toml";
/// File name of the uncommitted per-checkout binding.
pub const LOCAL_BINDING: &str = ".junto.local.toml";

/// The serialized shape of both binding files.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct BindingFile {
    /// Channel names (or raw ids) this checkout is bound to.
    #[serde(default)]
    pub channels: Vec<String>,
    /// This checkout's agent member code (`docs/adr/0017`) — only honored
    /// from the **local** (gitignored) file, so the secret never reaches the
    /// repo; `junto brief` relays it into agent context at session start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_code: Option<String>,
}

/// Read one binding file; a missing file is an empty binding.
fn read_binding(path: &Path) -> Result<BindingFile> {
    if !path.exists() {
        return Ok(BindingFile::default());
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// The checkout's effective channel binding: committed project channels, then
/// local additions, deduplicated in that order.
pub fn bound_channels(checkout_dir: &Path) -> Result<Vec<String>> {
    let mut channels = read_binding(&checkout_dir.join(PROJECT_BINDING))?.channels;
    for local in read_binding(&checkout_dir.join(LOCAL_BINDING))?.channels {
        if !channels.contains(&local) {
            channels.push(local);
        }
    }
    Ok(channels)
}

/// The agent member code from the **local** binding file only (the committed
/// file is in the repo; a secret there would sync — `docs/adr/0017`).
pub fn local_member_code(checkout_dir: &Path) -> Result<Option<String>> {
    Ok(read_binding(&checkout_dir.join(LOCAL_BINDING))?.member_code)
}

/// Write (or update) the **local** binding's member code — the code relay
/// (`docs/adr/0017`): the granted agent's code lands in the gitignored local
/// file so the session brief can hand it to the agent. Preserves any local
/// channel additions already present.
pub fn write_local_member_code(checkout_dir: &Path, code: &str) -> Result<()> {
    let path = checkout_dir.join(LOCAL_BINDING);
    let mut file = read_binding(&path)?;
    file.member_code = Some(code.to_string());
    let body = format!(
        "# This checkout's local junto binding (gitignored — see {PROJECT_BINDING} for the\n\
         # committed project binding). member_code is this checkout's agent code\n\
         # (docs/adr/0017); `junto brief` relays it into agent context at session start.\n{}",
        toml::to_string_pretty(&file).context("serializing local binding")?
    );
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// The outcome of seeding a checkout's member code from another checkout.
#[derive(Debug, PartialEq, Eq)]
pub enum WorktreeSeed {
    /// The member code was copied in from the source checkout.
    Seeded,
    /// The checkout already had a member code; it was left untouched.
    AlreadyPresent,
    /// The source checkout had no member code to copy.
    NothingToCopy,
}

/// Seed `checkout_dir`'s local member code from `source_dir`'s, if (and only
/// if) `checkout_dir` lacks one. This auto-heals a fresh git worktree: the
/// local binding is gitignored (`docs/adr/0017`), so `git worktree add` never
/// carries it over and the worktree starts with no agent code to write with.
/// Copying it from the primary worktree picks the right agent — same machine,
/// same operator — and is idempotent: an existing code is never overwritten,
/// and the worktree's own local channel additions are preserved.
pub fn seed_member_code_from(checkout_dir: &Path, source_dir: &Path) -> Result<WorktreeSeed> {
    if local_member_code(checkout_dir)?.is_some() {
        return Ok(WorktreeSeed::AlreadyPresent);
    }
    match local_member_code(source_dir)? {
        Some(code) => {
            write_local_member_code(checkout_dir, &code)?;
            Ok(WorktreeSeed::Seeded)
        }
        None => Ok(WorktreeSeed::NothingToCopy),
    }
}

/// Write the committed project binding.
pub fn write_project_binding(checkout_dir: &Path, channels: &[String]) -> Result<()> {
    let file = BindingFile {
        channels: channels.to_vec(),
        member_code: None,
    };
    let path = checkout_dir.join(PROJECT_BINDING);
    let body = format!(
        "# The project's ambient channel binding — which channel(s) sessions in this\n\
         # checkout consult and record into. Per-worktree additions go in\n\
         # {LOCAL_BINDING} (gitignored). See docs/adr/0014.\n{}",
        toml::to_string_pretty(&file).context("serializing channel binding")?
    );
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_files_mean_no_binding() {
        let dir = tempfile::tempdir().unwrap();
        assert!(bound_channels(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn local_binding_adds_to_the_project_binding() {
        let dir = tempfile::tempdir().unwrap();
        write_project_binding(dir.path(), &["junto-dev".into()]).unwrap();
        std::fs::write(
            dir.path().join(LOCAL_BINDING),
            "channels = [\"slice-8\", \"junto-dev\"]\n",
        )
        .unwrap();

        // Union, committed first, deduplicated.
        assert_eq!(
            bound_channels(dir.path()).unwrap(),
            vec!["junto-dev".to_string(), "slice-8".to_string()]
        );
    }

    #[test]
    fn project_binding_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        write_project_binding(dir.path(), &["alpha".into()]).unwrap();
        assert_eq!(
            bound_channels(dir.path()).unwrap(),
            vec!["alpha".to_string()]
        );
    }

    #[test]
    fn member_code_relay_preserves_local_channels() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(LOCAL_BINDING),
            "channels = [\"my-worktree-inquiry\"]\n",
        )
        .unwrap();

        write_local_member_code(dir.path(), "Abc123").unwrap();
        // Overwriting with a new code keeps working too.
        write_local_member_code(dir.path(), "Xyz789").unwrap();

        assert_eq!(
            local_member_code(dir.path()).unwrap().as_deref(),
            Some("Xyz789")
        );
        assert_eq!(
            bound_channels(dir.path()).unwrap(),
            vec!["my-worktree-inquiry".to_string()],
            "local channel additions survive the relay write"
        );
    }

    #[test]
    fn seed_copies_the_code_when_the_checkout_lacks_one() {
        let primary = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        write_local_member_code(primary.path(), "Abc123").unwrap();

        let outcome = seed_member_code_from(worktree.path(), primary.path()).unwrap();
        assert_eq!(outcome, WorktreeSeed::Seeded);
        assert_eq!(
            local_member_code(worktree.path()).unwrap().as_deref(),
            Some("Abc123")
        );
    }

    #[test]
    fn seed_is_a_no_op_when_the_checkout_already_has_a_code() {
        let primary = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        write_local_member_code(primary.path(), "Source").unwrap();
        write_local_member_code(worktree.path(), "Existing").unwrap();

        let outcome = seed_member_code_from(worktree.path(), primary.path()).unwrap();
        assert!(matches!(outcome, WorktreeSeed::AlreadyPresent));
        assert_eq!(
            local_member_code(worktree.path()).unwrap().as_deref(),
            Some("Existing"),
            "an existing code is never overwritten"
        );
    }

    #[test]
    fn seed_reports_nothing_to_copy_when_the_source_has_no_code() {
        let primary = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();

        let outcome = seed_member_code_from(worktree.path(), primary.path()).unwrap();
        assert!(matches!(outcome, WorktreeSeed::NothingToCopy));
        assert_eq!(local_member_code(worktree.path()).unwrap(), None);
    }

    #[test]
    fn seed_preserves_local_channels_in_the_worktree() {
        let primary = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        write_local_member_code(primary.path(), "Abc123").unwrap();
        std::fs::write(
            worktree.path().join(LOCAL_BINDING),
            "channels = [\"worktree-inquiry\"]\n",
        )
        .unwrap();

        seed_member_code_from(worktree.path(), primary.path()).unwrap();
        assert_eq!(
            bound_channels(worktree.path()).unwrap(),
            vec!["worktree-inquiry".to_string()],
            "the worktree's own local channel additions survive seeding"
        );
    }

    #[test]
    fn member_code_is_only_read_from_the_local_file() {
        // A code in the committed file would be a secret in the repo — it is
        // deliberately ignored (docs/adr/0017).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(PROJECT_BINDING),
            "channels = [\"x\"]\nmember_code = \"Leaked\"\n",
        )
        .unwrap();
        assert_eq!(local_member_code(dir.path()).unwrap(), None);
    }
}
