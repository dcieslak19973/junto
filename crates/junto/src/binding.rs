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
//!   e.g. the inquiry a worktree exists for.
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
}
