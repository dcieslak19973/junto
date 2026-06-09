//! Git-refs [`SubstrateProvider`] — junto's durable record in a local git repo.
//!
//! Stores a Channel's ledger in dedicated refs under **`refs/junto/*`**,
//! **partitioned by author** so concurrent writers never contend on the same
//! ref (hard constraint #3: append-only, no CRDT). Each author's ref
//!
//! ```text
//! refs/junto/<channel-id>/<author-slug>  ->  commit
//!   └─ entries.ndjson   (one canonical JSON line per entry)
//! ```
//!
//! points at a commit whose tree holds a single file, `entries.ndjson`: the
//! author's entries for that channel, one [`LedgerEntry::to_canonical_bytes`]
//! line each. Newline-framing is unambiguous because the canonical (JCS) form
//! contains no raw newline bytes (`docs/adr/0008`). Appending is a fast-forward
//! of the author's own ref; reading unions every author ref for the channel and
//! lets [`junto_kernel::Ledger::project`] impose the `(timestamp, author)` order.
//!
//! All git access shells out to the system `git` CLI (the assessed substrate
//! decision in CLAUDE.md) and touches **only the object DB and refs** — never a
//! working tree — so there is no `git status` pollution and Windows file
//! locking is sidestepped (git owns its own ref locks; we hold no files open).
//!
//! This is the **local** durable record. Syncing `refs/junto/*` to a forge
//! (push/fetch, capability flags, the Bitbucket fallback) is a separate, later
//! concern (`docs/adr/0009`).

use std::path::PathBuf;
use std::process::{Output, Stdio};

use junto_kernel::{ChannelId, Error, LedgerEntry, Result, SubstrateProvider};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// The all-zeros object id — asserts to `git update-ref` that a ref must not
/// yet exist (the optimistic guard when creating an author's first commit).
const ZERO_OID: &str = "0000000000000000000000000000000000000000";

/// The single file in each author commit's tree holding their entry log.
const LOG_FILE: &str = "entries.ndjson";

/// Bounded retries for the optimistic `update-ref` guard. Partition-by-author
/// makes contention rare (only the *same* author writing concurrently), so a
/// small bound is ample.
const MAX_APPEND_ATTEMPTS: usize = 8;

/// A [`SubstrateProvider`] backed by `refs/junto/*` in a local git repository.
///
/// Construct with [`GitRefsSubstrate::open`]. Assumes `git` is on `PATH` and
/// `repo` is (inside) a git repository.
#[derive(Debug, Clone)]
pub struct GitRefsSubstrate {
    repo: PathBuf,
}

impl GitRefsSubstrate {
    /// Open a substrate over the git repository at `repo`.
    pub fn open(repo: impl Into<PathBuf>) -> Self {
        Self { repo: repo.into() }
    }

    /// Run `git -C <repo> <args>`, optionally feeding `stdin` and extra `envs`.
    /// Returns the raw [`Output`] (caller inspects the exit status); errors only
    /// if the process could not be run.
    async fn git_raw(
        &self,
        args: &[&str],
        stdin: Option<&[u8]>,
        envs: &[(&str, &str)],
    ) -> Result<Output> {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.repo).args(args);
        for (key, value) in envs {
            cmd.env(key, value);
        }
        cmd.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| Error::Substrate(format!("could not run git: {e}")))?;
        if let Some(bytes) = stdin {
            let mut handle = child
                .stdin
                .take()
                .ok_or_else(|| Error::Substrate("git stdin was not captured".into()))?;
            handle
                .write_all(bytes)
                .await
                .map_err(|e| Error::Substrate(format!("writing git stdin failed: {e}")))?;
            // Drop closes the pipe so git sees EOF and proceeds.
            drop(handle);
        }
        child
            .wait_with_output()
            .await
            .map_err(|e| Error::Substrate(format!("waiting on git failed: {e}")))
    }

    /// Run git, requiring a zero exit; returns stdout bytes or an error carrying
    /// stderr.
    async fn git(
        &self,
        args: &[&str],
        stdin: Option<&[u8]>,
        envs: &[(&str, &str)],
    ) -> Result<Vec<u8>> {
        let out = self.git_raw(args, stdin, envs).await?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(Error::Substrate(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            )))
        }
    }

    /// The commit a ref currently points at, or `None` if it does not exist.
    async fn ref_tip(&self, refname: &str) -> Result<Option<String>> {
        let out = self
            .git_raw(&["rev-parse", "--verify", "-q", refname], None, &[])
            .await?;
        if out.status.success() {
            Ok(Some(trimmed(out.stdout)?))
        } else {
            Ok(None)
        }
    }
}

impl SubstrateProvider for GitRefsSubstrate {
    async fn append(&mut self, entry: LedgerEntry) -> Result<()> {
        let refname = format!(
            "refs/junto/{}/{}",
            entry.channel,
            author_slug(&entry.author.email)
        );

        // The line this append adds: canonical bytes + a framing newline.
        let mut line = entry.to_canonical_bytes()?;
        line.push(b'\n');

        // Attribute the commit to the real author and pin its dates to the
        // entry timestamp, so commits are deterministic and `git log` is honest.
        // `@<seconds>` is git's explicit epoch form (plain "<secs> +0000" is
        // rejected for small values); UTC keeps commit oids deterministic.
        let date = format!("@{} +0000", entry.timestamp.as_millis().div_euclid(1000));
        let name = entry.author.display_name.clone();
        let email = entry.author.email.clone();
        let envs: [(&str, &str); 6] = [
            ("GIT_AUTHOR_NAME", &name),
            ("GIT_AUTHOR_EMAIL", &email),
            ("GIT_AUTHOR_DATE", &date),
            ("GIT_COMMITTER_NAME", &name),
            ("GIT_COMMITTER_EMAIL", &email),
            ("GIT_COMMITTER_DATE", &date),
        ];
        let message = format!("junto entry {}", entry.id);

        for _ in 0..MAX_APPEND_ATTEMPTS {
            let tip = self.ref_tip(&refname).await?;

            // Read the author's current log (empty on first write), append the line.
            let mut contents = match &tip {
                Some(_) => {
                    self.git(
                        &["cat-file", "-p", &format!("{refname}:{LOG_FILE}")],
                        None,
                        &[],
                    )
                    .await?
                }
                None => Vec::new(),
            };
            contents.extend_from_slice(&line);

            // blob -> tree -> commit, all in the object DB (no working tree).
            // `--no-filters` is already implied for `--stdin` without `--path`,
            // but we state it so no clean/smudge filter (notably `core.autocrlf`)
            // can ever rewrite the bytes of the record. See the autocrlf test.
            let blob = trimmed(
                self.git(
                    &["hash-object", "-w", "--no-filters", "--stdin"],
                    Some(&contents),
                    &[],
                )
                .await?,
            )?;
            let tree = trimmed(
                self.git(
                    &["mktree"],
                    Some(format!("100644 blob {blob}\t{LOG_FILE}\n").as_bytes()),
                    &[],
                )
                .await?,
            )?;

            let mut commit_args = vec!["commit-tree", tree.as_str()];
            if let Some(parent) = &tip {
                commit_args.push("-p");
                commit_args.push(parent.as_str());
            }
            commit_args.push("-m");
            commit_args.push(message.as_str());
            let commit = trimmed(self.git(&commit_args, None, &envs).await?)?;

            // Move the ref, guarded by the value we read — losing the race means
            // a concurrent same-author write landed; re-read and retry.
            let old = tip.as_deref().unwrap_or(ZERO_OID);
            let update = self
                .git_raw(&["update-ref", &refname, &commit, old], None, &[])
                .await?;
            if update.status.success() {
                return Ok(());
            }
        }

        Err(Error::Substrate(format!(
            "update-ref for {refname} kept losing races after {MAX_APPEND_ATTEMPTS} attempts"
        )))
    }

    async fn entries(&self, channel: &ChannelId) -> Result<Vec<LedgerEntry>> {
        let prefix = format!("refs/junto/{channel}/");
        let refs_out = self
            .git(&["for-each-ref", "--format=%(refname)", &prefix], None, &[])
            .await?;
        let refs_text = String::from_utf8(refs_out)
            .map_err(|e| Error::Substrate(format!("for-each-ref output not utf-8: {e}")))?;

        let mut entries = Vec::new();
        for refname in refs_text.lines().filter(|l| !l.is_empty()) {
            let blob = self
                .git(
                    &["cat-file", "-p", &format!("{refname}:{LOG_FILE}")],
                    None,
                    &[],
                )
                .await?;
            for line in blob.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
                entries.push(LedgerEntry::from_canonical_bytes(line)?);
            }
        }
        Ok(entries)
    }
}

/// Decode git command output (an oid or ref name) into a trimmed `String`.
fn trimmed(bytes: Vec<u8>) -> Result<String> {
    let text = String::from_utf8(bytes)
        .map_err(|e| Error::Substrate(format!("git output not utf-8: {e}")))?;
    Ok(text.trim().to_string())
}

/// A ref-safe slug for an author email. The email is also stored inside each
/// entry's JSON, so the slug need only be **safe and unique**, not reversible.
///
/// We keep only ASCII alphanumerics, `-`, and `_`, percent-encoding everything
/// else (incl. `.` and `@`). Dropping `.` is deliberate — it removes any chance
/// of a forbidden `..` sequence or a leading/trailing dot in a ref component.
fn author_slug(email: &str) -> String {
    let mut slug = String::with_capacity(email.len());
    for &byte in email.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => slug.push(byte as char),
            _ => slug.push_str(&format!("%{byte:02X}")),
        }
    }
    slug
}

#[cfg(test)]
mod tests {
    use super::*;
    use junto_kernel::{EntryPayload, InMemorySubstrate, Ledger, Member, Standing, Timestamp};
    use std::path::Path;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    /// A fresh git repo in a tempdir, plus a substrate over it.
    fn init_repo() -> (TempDir, GitRefsSubstrate) {
        let dir = tempfile::tempdir().expect("tempdir");
        let status = StdCommand::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .expect("run git init");
        assert!(status.success(), "git init failed");
        let substrate = GitRefsSubstrate::open(dir.path().to_path_buf());
        (dir, substrate)
    }

    fn assertion(author: &Member, ts: i64, statement: &str) -> LedgerEntry {
        LedgerEntry {
            id: junto_kernel::EntryId::new(),
            channel: junto_kernel::ChannelId::new(),
            author: author.clone(),
            timestamp: Timestamp::from_millis(ts),
            payload: EntryPayload::Assertion {
                statement: statement.into(),
                rationale: "because".into(),
                provenance: vec![],
            },
        }
    }

    fn git_out(repo: &Path, args: &[&str]) -> String {
        let out = StdCommand::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("utf8")
    }

    #[tokio::test]
    async fn round_trips_through_a_fresh_substrate() {
        let (dir, mut substrate) = init_repo();
        let ada = Member::human("Ada", "ada@example.com");
        let e1 = assertion(&ada, 10, "first");
        let mut e2 = assertion(&ada, 20, "second");
        e2.channel = e1.channel; // same channel
        substrate.append(e1.clone()).await.unwrap();
        substrate.append(e2.clone()).await.unwrap();

        // A brand-new substrate over the same repo proves there is no in-memory
        // state — the record is durable on disk.
        let reopened = GitRefsSubstrate::open(dir.path().to_path_buf());
        let mut got = reopened.entries(&e1.channel).await.unwrap();
        got.sort_by_key(|e| e.timestamp.as_millis());
        assert_eq!(got, vec![e1, e2]);
    }

    #[tokio::test]
    async fn appends_accumulate_for_one_author() {
        let (_dir, mut substrate) = init_repo();
        let ada = Member::human("Ada", "ada@example.com");
        let e1 = assertion(&ada, 10, "first");
        let mut e2 = assertion(&ada, 20, "second");
        e2.channel = e1.channel;
        substrate.append(e1.clone()).await.unwrap();
        substrate.append(e2.clone()).await.unwrap();
        let got = substrate.entries(&e1.channel).await.unwrap();
        assert_eq!(got.len(), 2, "second append must not clobber the first");
    }

    #[tokio::test]
    async fn partitions_by_author_into_separate_refs() {
        let (dir, mut substrate) = init_repo();
        let ada = Member::human("Ada", "ada@example.com");
        let bob = Member::agent("Bob", "bob@example.com");
        let a = assertion(&ada, 10, "from ada");
        let mut b = assertion(&bob, 20, "from bob");
        b.channel = a.channel;
        substrate.append(a.clone()).await.unwrap();
        substrate.append(b.clone()).await.unwrap();

        // Two distinct author refs exist for the one channel.
        let refs = git_out(
            dir.path(),
            &[
                "for-each-ref",
                "--format=%(refname)",
                &format!("refs/junto/{}/", a.channel),
            ],
        );
        assert_eq!(refs.lines().count(), 2, "one ref per author");

        // The union round-trips both.
        let got = substrate.entries(&a.channel).await.unwrap();
        assert_eq!(got.len(), 2);
    }

    #[tokio::test]
    async fn scopes_entries_to_their_channel() {
        let (_dir, mut substrate) = init_repo();
        let ada = Member::human("Ada", "ada@example.com");
        let a = assertion(&ada, 10, "channel A");
        let b = assertion(&ada, 20, "channel B"); // different (fresh) channel
        substrate.append(a.clone()).await.unwrap();
        substrate.append(b.clone()).await.unwrap();
        assert_eq!(substrate.entries(&a.channel).await.unwrap(), vec![a]);
        assert_eq!(substrate.entries(&b.channel).await.unwrap(), vec![b]);
    }

    #[tokio::test]
    async fn never_touches_the_working_tree() {
        let (dir, mut substrate) = init_repo();
        let ada = Member::human("Ada", "ada@example.com");
        substrate.append(assertion(&ada, 10, "x")).await.unwrap();
        let status = git_out(dir.path(), &["status", "--porcelain"]);
        assert!(
            status.trim().is_empty(),
            "working tree must stay clean: {status:?}"
        );
    }

    #[tokio::test]
    async fn stores_newline_framed_canonical_json() {
        let (dir, mut substrate) = init_repo();
        let ada = Member::human("Ada", "ada@example.com");
        let e = assertion(&ada, 10, "x");
        substrate.append(e.clone()).await.unwrap();
        let refname = format!(
            "refs/junto/{}/{}",
            e.channel,
            author_slug("ada@example.com")
        );
        let blob = git_out(dir.path(), &["show", &format!("{refname}:{LOG_FILE}")]);
        assert!(blob.ends_with('\n'));
        // JCS sorts keys, so the line starts with "author".
        assert!(blob.starts_with("{\"author\":"), "got: {blob}");
    }

    #[tokio::test]
    async fn core_autocrlf_does_not_corrupt_the_record() {
        // The exact CLAUDE.md line-ending hazard, at the code that writes the
        // record: with core.autocrlf=true (a common Windows default) a
        // clean/smudge filter could rewrite newlines. We dodge it structurally
        // (hash-object --stdin --no-filters), and pin it here. CI runners may
        // not set autocrlf=true, so this local test is the real proof.
        let dir = tempfile::tempdir().expect("tempdir");
        for args in [
            ["init", "-q"].as_slice(),
            &["config", "core.autocrlf", "true"],
        ] {
            let ok = StdCommand::new("git")
                .args(args)
                .current_dir(dir.path())
                .status()
                .expect("run git")
                .success();
            assert!(ok, "git {args:?} failed");
        }
        let mut substrate = GitRefsSubstrate::open(dir.path().to_path_buf());

        // A rationale whose content carries a CRLF — canonical JSON escapes it
        // to the four chars \r \n, leaving no raw newline for a filter to touch
        // beyond our single framing \n.
        let ada = Member::human("Ada", "ada@example.com");
        let entry = LedgerEntry {
            id: junto_kernel::EntryId::new(),
            channel: junto_kernel::ChannelId::new(),
            author: ada,
            timestamp: Timestamp::from_millis(10),
            payload: EntryPayload::Assertion {
                statement: "x".into(),
                rationale: "line one\r\nline two".into(),
                provenance: vec![],
            },
        };
        substrate.append(entry.clone()).await.unwrap();

        // Bytes round-trip exactly: the CRLF in the rationale survives.
        let got = substrate.entries(&entry.channel).await.unwrap();
        assert_eq!(got, vec![entry]);

        // And autocrlf did not inject a stray raw newline: still exactly one
        // framed entry line in the stored log.
        let refname = format!(
            "refs/junto/{}/{}",
            got[0].channel,
            author_slug("ada@example.com")
        );
        let blob = git_out(dir.path(), &["show", &format!("{refname}:{LOG_FILE}")]);
        assert_eq!(blob.matches('\n').count(), 1, "exactly one framed line");
    }

    #[tokio::test]
    async fn drives_the_generic_ledger_like_the_in_memory_backend() {
        // The same domain logic (append + project) must run over the git backend
        // unchanged and agree with InMemorySubstrate on order and standings.
        let ada = Member::human("Ada", "ada@example.com");
        let bob = Member::human("Bob", "bob@example.com");
        let claim = assertion(&ada, 100, "the claim");
        let mut ratify = LedgerEntry {
            id: junto_kernel::EntryId::new(),
            channel: claim.channel,
            author: bob.clone(),
            timestamp: Timestamp::from_millis(200),
            payload: EntryPayload::Ratification {
                target: claim.id,
                rationale: "confirmed".into(),
            },
        };
        ratify.channel = claim.channel;

        // git-backed
        let (_dir, git) = init_repo();
        let mut git_ledger = Ledger::new(git);
        git_ledger.append(claim.clone()).await.unwrap();
        git_ledger.append(ratify.clone()).await.unwrap();
        let git_view = git_ledger.project(&claim.channel).await.unwrap();

        // in-memory reference
        let mut mem_ledger = Ledger::new(InMemorySubstrate::new());
        mem_ledger.append(claim.clone()).await.unwrap();
        mem_ledger.append(ratify.clone()).await.unwrap();
        let mem_view = mem_ledger.project(&claim.channel).await.unwrap();

        // Same canonical order and same derived standing.
        assert_eq!(
            git_view.entries.iter().map(|e| e.id).collect::<Vec<_>>(),
            mem_view.entries.iter().map(|e| e.id).collect::<Vec<_>>(),
        );
        assert_eq!(git_view.standing(&claim.id), Some(Standing::Ratified));
        assert_eq!(git_view.standing(&claim.id), mem_view.standing(&claim.id));
    }
}
