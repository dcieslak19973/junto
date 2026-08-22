//! The **Mount** store — how *this* machine resolves a Subject to a path.
//!
//! The counterpart to the kernel's `Subject` (spec §1). A Subject is portable
//! and lives in the ledger; a Mount is a machine fact and never leaves this
//! disk, exactly as the Workspace store it replaces never did
//! (`domain-model.md:32`). Unlike that store there is **no `.git`
//! requirement**: a document subject mounts to a directory or file and simply
//! reports fewer capabilities.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use junto_kernel::{Subject, Uri};
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

/// Where this machine keeps the given subject, if anywhere.
// No production caller yet: the single-subject lookup this exists for
// (checking a specific Subject's mount before, say, re-mounting it) has no
// call site until a Subject-attachment/re-mount surface exists. Exercised by
// this module's own tests; kept public per the interface this module was
// specified against.
#[allow(dead_code)]
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

/// Remember (or update) where this machine keeps a subject.
// No production caller yet, deliberately: writing a mount requires a
// Subject to key it on, and nothing in this task attaches one — see
// `crate::web`'s `required_mount`, which refuses rather than invent one.
// `Host::attach_subject` (a follow-up task) is the actual write path; until
// then this is exercised only by this module's own tests, kept public per
// the interface this module was specified against.
#[allow(dead_code)]
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
/// specific subject should use `mounts_for`/`mount_path` instead, which stay
/// scoped to a channel's actual subjects rather than every mount ever made.
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
        let repo = uri("git+https://example.com/a.git");
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
        let mounted = uri("git+https://example.com/a.git");
        let dir = tempfile::tempdir().unwrap();
        remember_mount(home.path(), &mounted, dir.path()).unwrap();

        let subjects = vec![
            Subject::new(SubjectKind::Repo, mounted.clone()),
            Subject::new(SubjectKind::Repo, uri("git+https://example.com/never.git")),
        ];
        let mounts = mounts_for(home.path(), &subjects).unwrap();
        assert_eq!(mounts.len(), 1, "the unmounted subject is simply absent");
        assert_eq!(mounts[0].uri, mounted);
    }

    #[test]
    fn all_mounts_lists_every_remembered_mount_regardless_of_subject() {
        let home = HomeGuard::new();
        let a = uri("git+https://example.com/a.git");
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
}
