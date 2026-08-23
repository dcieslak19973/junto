//! The **Mount** store — how *this* machine resolves a Subject to a path.
//!
//! The counterpart to the kernel's `Subject` (spec §1). A Subject is portable
//! and lives in the ledger; a Mount is a machine fact and never leaves this
//! disk, exactly as the Workspace store it replaces never did
//! (`domain-model.md:32`). Unlike that store there is **no `.git`
//! requirement**: a document subject mounts to a directory or file and simply
//! reports fewer capabilities.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use junto_kernel::{Subject, SubjectKind, Uri};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
struct MountsFile {
    #[serde(default)]
    mounts: Vec<MountRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
struct MountRecord {
    uri: String,
    path: PathBuf,
}

/// One subject, resolved on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// The subject this resolves.
    pub uri: Uri,
    /// Where it lives here.
    pub path: PathBuf,
}

/// The path to this machine's mount store, under its junto home.
fn mounts_path(junto_home: &Path) -> PathBuf {
    junto_home.join("mounts.toml")
}

/// Read the mount store, or an empty one if it has never been written.
fn read_mounts(junto_home: &Path) -> Result<MountsFile> {
    let path = mounts_path(junto_home);
    if !path.exists() {
        return Ok(MountsFile::default());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Where this machine keeps the given subject, if anywhere — the direct
/// read counterpart to [`remember_mount`]. No production caller resolves a
/// session's workdir this way any more (`session_workdir` — the sole
/// authority, capability-aware — replaced the last one, finding 8 of the
/// final review); kept `#[cfg(test)]` as the read/write round-trip these
/// modules' own tests exercise directly.
#[cfg(test)]
pub fn mount_path(junto_home: &Path, uri: &Uri) -> Result<Option<PathBuf>> {
    Ok(read_mounts(junto_home)?
        .mounts
        .into_iter()
        .find(|record| record.uri == uri.as_str())
        .map(|record| record.path))
}

/// Resolve every subject this machine can. Subjects with no mount are
/// **skipped, not an error** — a teammate may hold a checkout you do not, and
/// the channel still reads perfectly well without it.
pub fn mounts_for(junto_home: &Path, subjects: &[Subject]) -> Result<Vec<Mount>> {
    let file = read_mounts(junto_home)?;
    Ok(subjects
        .iter()
        .filter_map(|subject| {
            file.mounts
                .iter()
                .find(|record| record.uri == subject.uri.as_str())
                .map(|record| Mount {
                    uri: subject.uri.clone(),
                    path: record.path.clone(),
                })
        })
        .collect())
}

/// Remember (or update) where this machine keeps a subject. The write half
/// of the Mount store: `crate::web`'s `launch_session` calls this *before*
/// `Host::attach_subject`, not after. `remember_mount` needs only the uri
/// and the path, both already in hand, so it moves ahead of the append and
/// the one irreversible step — the `SubjectAttached` entry — runs last among
/// what can still fail (see the comment in `web.rs`'s `launch_session`).
pub fn remember_mount(junto_home: &Path, uri: &Uri, path: &Path) -> Result<()> {
    let path = dunce::canonicalize(path)
        .with_context(|| format!("mount path {} not found", path.display()))?;
    let mut file = read_mounts(junto_home)?;
    match file
        .mounts
        .iter_mut()
        .find(|record| record.uri == uri.as_str())
    {
        Some(record) => record.path = path,
        None => file.mounts.push(MountRecord {
            uri: uri.as_str().to_owned(),
            path,
        }),
    }
    let target = mounts_path(junto_home);
    std::fs::create_dir_all(junto_home)
        .with_context(|| format!("creating {}", junto_home.display()))?;
    std::fs::write(
        &target,
        toml::to_string_pretty(&file).context("serializing mounts")?,
    )
    .with_context(|| format!("writing {}", target.display()))?;
    Ok(())
}

/// Every mount this machine has, regardless of channel or subject.
///
/// The one caller is a surface that lists "everything available here" for
/// picking (the launch form's suggestions) — ordinary code resolving a
/// specific subject should use `mounts_for` instead, which stays scoped to
/// a channel's actual subjects rather than every mount ever made.
pub fn all_mounts(junto_home: &Path) -> Result<Vec<Mount>> {
    read_mounts(junto_home)?
        .mounts
        .into_iter()
        .map(|record| {
            let uri = Uri::new(&record.uri)
                .with_context(|| format!("mounts.toml has an invalid uri: {}", record.uri))?;
            Ok(Mount {
                uri,
                path: record.path,
            })
        })
        .collect()
}

/// What a Subject affords, on this machine, right now.
///
/// **Never recorded.** Capabilities vary by machine — one host has the
/// checkout and the credentials, another has neither — so putting them in the
/// ledger would smuggle machine facts into the record (spec §1). They are
/// recomputed at use time from the kind and the mount, and resolve against the
/// **executing host**, not the viewing human (spec §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    /// Fetch its current state.
    Read,
    /// Receive change events.
    Watch,
    /// Attach a span that survives the content moving.
    Anchor,
    /// Produce a mechanical before/after.
    Diff,
    /// Run an agent session inside it.
    Execute,
    /// Write back to it — always through a gate.
    Mutate,
}

/// The capability set for a subject on this machine.
#[must_use]
pub fn capabilities(subject: &Subject, mount: Option<&Mount>) -> BTreeSet<Capability> {
    let mut caps = BTreeSet::new();
    // Reading is the floor: a URI is enough to fetch or open something.
    caps.insert(Capability::Read);
    if mount.is_none() {
        return caps;
    }
    caps.insert(Capability::Watch);
    caps.insert(Capability::Anchor);
    caps.insert(Capability::Mutate);
    match subject.kind {
        SubjectKind::Repo => {
            caps.insert(Capability::Diff);
            caps.insert(Capability::Execute);
        }
        // A document has no working tree to run in and no mechanical diff;
        // its provenance is a content digest instead (spec §1).
        SubjectKind::Document => {}
    }
    caps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::test_home::HomeGuard;
    use junto_kernel::{Subject, SubjectKind, Uri};

    fn uri(text: &str) -> Uri {
        Uri::new(text).expect("valid uri")
    }

    #[test]
    fn a_mount_is_remembered_by_uri_and_updated_in_place() {
        let home = HomeGuard::new();
        let repo = uri("https://example.com/a.git");
        assert!(mount_path(home.path(), &repo).unwrap().is_none());

        let first = tempfile::tempdir().unwrap();
        remember_mount(home.path(), &repo, first.path()).unwrap();
        assert_eq!(
            mount_path(home.path(), &repo).unwrap().unwrap(),
            dunce::canonicalize(first.path()).unwrap()
        );

        let second = tempfile::tempdir().unwrap();
        remember_mount(home.path(), &repo, second.path()).unwrap();
        assert_eq!(
            mount_path(home.path(), &repo).unwrap().unwrap(),
            dunce::canonicalize(second.path()).unwrap(),
            "remembering the same uri replaces rather than duplicating"
        );
    }

    #[test]
    fn a_non_git_directory_is_a_perfectly_good_mount() {
        let home = HomeGuard::new();
        let notes = tempfile::tempdir().unwrap();
        let doc = uri("file:///notes/spec.md");
        remember_mount(home.path(), &doc, notes.path())
            .expect("a document mount must not require a .git directory");
        assert!(mount_path(home.path(), &doc).unwrap().is_some());
    }

    #[test]
    fn unmounted_subjects_are_skipped_rather_than_erroring() {
        let home = HomeGuard::new();
        let mounted = uri("https://example.com/a.git");
        let dir = tempfile::tempdir().unwrap();
        remember_mount(home.path(), &mounted, dir.path()).unwrap();

        let subjects = vec![
            Subject::new(SubjectKind::Repo, mounted.clone()),
            Subject::new(SubjectKind::Repo, uri("https://example.com/never.git")),
        ];
        let mounts = mounts_for(home.path(), &subjects).unwrap();
        assert_eq!(mounts.len(), 1, "the unmounted subject is simply absent");
        assert_eq!(mounts[0].uri, mounted);
    }

    #[test]
    fn all_mounts_lists_every_remembered_mount_regardless_of_subject() {
        let home = HomeGuard::new();
        let a = uri("https://example.com/a.git");
        let b = uri("file:///notes/spec.md");
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        remember_mount(home.path(), &a, dir_a.path()).unwrap();
        remember_mount(home.path(), &b, dir_b.path()).unwrap();

        let mut mounts = all_mounts(home.path()).unwrap();
        mounts.sort_by(|x, y| x.uri.as_str().cmp(y.uri.as_str()));
        assert_eq!(
            mounts,
            vec![
                Mount {
                    uri: b,
                    path: dunce::canonicalize(dir_b.path()).unwrap(),
                },
                Mount {
                    uri: a,
                    path: dunce::canonicalize(dir_a.path()).unwrap(),
                },
            ],
            "every remembered mount must be listed, with no subject required"
        );
    }

    #[test]
    fn a_mounted_repo_can_do_everything_and_an_unmounted_one_can_only_be_read() {
        let repo = Subject::new(SubjectKind::Repo, uri("https://example.com/a.git"));
        let dir = tempfile::tempdir().unwrap();
        let mount = Mount {
            uri: repo.uri.clone(),
            path: dir.path().to_path_buf(),
        };

        let mounted = capabilities(&repo, Some(&mount));
        for expected in [
            Capability::Read,
            Capability::Watch,
            Capability::Anchor,
            Capability::Diff,
            Capability::Execute,
            Capability::Mutate,
        ] {
            assert!(mounted.contains(&expected), "missing {expected:?}");
        }

        let unmounted = capabilities(&repo, None);
        assert_eq!(
            unmounted,
            [Capability::Read].into_iter().collect(),
            "without a mount there is nothing to run in, diff, or write back to"
        );
    }

    #[test]
    fn a_document_is_never_executable_even_when_mounted() {
        let doc = Subject::new(SubjectKind::Document, uri("file:///notes/spec.md"));
        let dir = tempfile::tempdir().unwrap();
        let mount = Mount {
            uri: doc.uri.clone(),
            path: dir.path().to_path_buf(),
        };
        let caps = capabilities(&doc, Some(&mount));
        assert!(!caps.contains(&Capability::Execute));
        assert!(!caps.contains(&Capability::Diff));
        assert!(caps.contains(&Capability::Anchor));
        assert!(caps.contains(&Capability::Mutate));
    }
}
