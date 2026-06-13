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
//! Beyond local storage, [`GitRefsSubstrate::sync`] exchanges a channel's refs
//! with any git remote (the forge-as-hub model, `docs/adr/0011`): fetch every
//! author ref, reconcile each one locally (create / fast-forward / union-merge
//! on divergence), then push. There is no merge *logic* — a union of immutable
//! entries deduplicated by id is the whole reconciliation, which is exactly
//! what the no-CRDT design promised. Forge capability flags (the Bitbucket
//! `refs/heads/junto/*` fallback) remain deferred (`docs/adr/0009`).

use std::collections::HashSet;
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

/// Bounded fetch→reconcile→push cycles for [`GitRefsSubstrate::sync`]: a push
/// is rejected (and the cycle repeats) only when the remote advanced between
/// our fetch and our push.
const MAX_SYNC_ATTEMPTS: usize = 4;

/// The fixed identity stamped on reconciliation merge commits. Mechanical
/// merges have no human author; a fixed identity (plus sorted parents and a
/// date pinned to the newer parent) makes the merge commit **deterministic**,
/// so two replicas reconciling the same divergence mint the *same* commit and
/// converge instead of ping-ponging new merges at each other.
const SYNC_NAME: &str = "junto sync";
const SYNC_EMAIL: &str = "sync@junto.invalid";

/// How one fetch→reconcile→push cycle ended (private detail of `sync`).
enum PushOutcome {
    /// Every local ref is on the remote.
    Done,
    /// The remote advanced since our fetch; carry the stderr and re-cycle.
    Rejected(String),
}

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
        // Terminal-less (CLAUDE.md): the host shells out to git constantly
        // (every read, append, push, fetch, sync). On Windows each spawn would
        // flash a console window; suppress it. This is the single chokepoint
        // for all production git, so the whole substrate goes quiet.
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

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

    /// The full refnames under `refs/junto/<channel>/` in this repo.
    async fn channel_refs(&self, channel: &ChannelId) -> Result<Vec<String>> {
        let prefix = format!("refs/junto/{channel}/");
        let out = self
            .git(&["for-each-ref", "--format=%(refname)", &prefix], None, &[])
            .await?;
        let text = String::from_utf8(out)
            .map_err(|e| Error::Substrate(format!("for-each-ref output not utf-8: {e}")))?;
        Ok(text
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// The entry-log bytes at `rev` (a commit oid or refname).
    async fn log_at(&self, rev: &str) -> Result<Vec<u8>> {
        self.git(&["cat-file", "-p", &format!("{rev}:{LOG_FILE}")], None, &[])
            .await
    }

    /// Write `contents` as the single-file log tree and commit it with the
    /// given parents, identity envs, and message; returns the commit oid.
    ///
    /// blob → tree → commit, all in the object DB (no working tree).
    /// `--no-filters` is already implied for `--stdin` without `--path`, but we
    /// state it so no clean/smudge filter (notably `core.autocrlf`) can ever
    /// rewrite the bytes of the record. See the autocrlf test.
    async fn commit_log(
        &self,
        contents: &[u8],
        parents: &[&str],
        envs: &[(&str, &str)],
        message: &str,
    ) -> Result<String> {
        let blob = trimmed(
            self.git(
                &["hash-object", "-w", "--no-filters", "--stdin"],
                Some(contents),
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
        for parent in parents {
            commit_args.push("-p");
            commit_args.push(parent);
        }
        commit_args.push("-m");
        commit_args.push(message);
        trimmed(self.git(&commit_args, None, envs).await?)
    }

    /// Whether `ancestor` is an ancestor of (or equal to) `descendant`.
    async fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
        let out = self
            .git_raw(
                &["merge-base", "--is-ancestor", ancestor, descendant],
                None,
                &[],
            )
            .await?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(Error::Substrate(format!(
                "merge-base --is-ancestor {ancestor} {descendant} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))),
        }
    }

    /// Synchronize one channel's record with `remote` (a git remote name, URL,
    /// or path — anything the system git accepts): **fetch** every
    /// `refs/junto/<channel>/*` author ref, **reconcile** each into the local
    /// record, then **push** every local author ref back.
    ///
    /// Reconciliation per ref: create it if absent, fast-forward if the remote
    /// is ahead, keep ours if we are ahead, and on true divergence (the same
    /// author wrote on two machines) mint a **union-merge commit** — the
    /// deduplicated union of both logs in canonical order, with both tips as
    /// parents (`docs/adr/0011`). Entries are immutable and identified by id,
    /// so the union *is* the merge; no conflict is possible by construction.
    ///
    /// A push rejected because the remote advanced mid-cycle re-runs the
    /// fetch→reconcile→push loop (bounded). Safe to call with nothing local,
    /// nothing remote, or both.
    ///
    /// # Errors
    /// Any failing git command surfaces as [`Error::Substrate`]; exhausting the
    /// retry bound reports the last push rejection.
    pub async fn sync(&mut self, remote: &str, channel: &ChannelId) -> Result<()> {
        let mut last_rejection = String::new();
        for _ in 0..MAX_SYNC_ATTEMPTS {
            self.fetch_and_reconcile(remote, channel).await?;
            match self.push_channel(remote, channel).await? {
                PushOutcome::Done => return Ok(()),
                PushOutcome::Rejected(stderr) => last_rejection = stderr,
            }
        }
        Err(Error::Substrate(format!(
            "sync with {remote} kept being outpaced after {MAX_SYNC_ATTEMPTS} attempts: \
             {last_rejection}"
        )))
    }

    /// Fetch the remote's author refs for `channel` and reconcile each into
    /// the local record. No local ref is moved except through the guarded
    /// reconcile, so a concurrent local append cannot be clobbered.
    async fn fetch_and_reconcile(&self, remote: &str, channel: &ChannelId) -> Result<()> {
        let pattern = format!("refs/junto/{channel}/*");
        let listing = self
            .git(&["ls-remote", "--refs", remote, &pattern], None, &[])
            .await?;
        let listing = String::from_utf8(listing)
            .map_err(|e| Error::Substrate(format!("ls-remote output not utf-8: {e}")))?;
        let tips: Vec<(&str, &str)> = listing
            .lines()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                Some((parts.next()?, parts.next()?))
            })
            .collect();
        if tips.is_empty() {
            return Ok(());
        }

        // One fetch brings every remote tip's objects into the local object DB.
        // Deliberately no destination refspec: we move local refs ourselves,
        // under the optimistic guard in `reconcile_ref`.
        let mut fetch_args = vec!["fetch", "--quiet", remote];
        fetch_args.extend(tips.iter().map(|(_, refname)| *refname));
        self.git(&fetch_args, None, &[]).await?;

        for (oid, refname) in &tips {
            self.reconcile_ref(refname, oid).await?;
        }
        Ok(())
    }

    /// Move the local `refname` to incorporate `remote_oid`, retrying the
    /// guarded update if a concurrent local append moves the ref under us.
    async fn reconcile_ref(&self, refname: &str, remote_oid: &str) -> Result<()> {
        let mut last_stderr = String::new();
        for _ in 0..MAX_APPEND_ATTEMPTS {
            let local = self.ref_tip(refname).await?;
            let (new, old) = match &local {
                // Unknown ref: adopt the remote tip (guard: must still not exist).
                None => (remote_oid.to_string(), ZERO_OID.to_string()),
                Some(local_oid) if local_oid == remote_oid => return Ok(()),
                Some(local_oid) => {
                    if self.is_ancestor(remote_oid, local_oid).await? {
                        // We already contain the remote history.
                        return Ok(());
                    } else if self.is_ancestor(local_oid, remote_oid).await? {
                        // Remote is strictly ahead: fast-forward.
                        (remote_oid.to_string(), local_oid.clone())
                    } else {
                        // True divergence: same author wrote on two machines.
                        let merge = self.union_merge(local_oid, remote_oid).await?;
                        (merge, local_oid.clone())
                    }
                }
            };
            let update = self
                .git_raw(&["update-ref", refname, &new, &old], None, &[])
                .await?;
            if update.status.success() {
                return Ok(());
            }
            last_stderr = String::from_utf8_lossy(&update.stderr).trim().to_string();
        }
        Err(Error::Substrate(format!(
            "reconciling {refname} failed after {MAX_APPEND_ATTEMPTS} attempts \
             (a contended ref, or a non-race fault): {last_stderr}"
        )))
    }

    /// Mint the union-merge commit of two diverged log tips: the deduplicated
    /// union of both logs, in canonical order, with both tips as parents.
    ///
    /// Deterministic by construction — sorted parents, fixed identity, date
    /// pinned to the newer parent — so both replicas mint the same commit oid
    /// from the same divergence and converge immediately.
    async fn union_merge(&self, ours: &str, theirs: &str) -> Result<String> {
        let mut seen = HashSet::new();
        let mut entries: Vec<LedgerEntry> = Vec::new();
        for rev in [ours, theirs] {
            let log = self.log_at(rev).await?;
            for line in log.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
                let entry = LedgerEntry::from_canonical_bytes(line)?;
                if seen.insert(entry.id) {
                    entries.push(entry);
                }
            }
        }
        entries.sort_by(LedgerEntry::canonical_cmp);

        let mut contents = Vec::new();
        for entry in &entries {
            contents.extend_from_slice(&entry.to_canonical_bytes()?);
            contents.push(b'\n');
        }

        let mut parents = [ours, theirs];
        parents.sort_unstable();
        let date = format!("@{} +0000", self.newest_commit_epoch(&parents).await?);
        let envs: [(&str, &str); 6] = [
            ("GIT_AUTHOR_NAME", SYNC_NAME),
            ("GIT_AUTHOR_EMAIL", SYNC_EMAIL),
            ("GIT_AUTHOR_DATE", &date),
            ("GIT_COMMITTER_NAME", SYNC_NAME),
            ("GIT_COMMITTER_EMAIL", SYNC_EMAIL),
            ("GIT_COMMITTER_DATE", &date),
        ];
        self.commit_log(
            &contents,
            &parents,
            &envs,
            "junto sync: union of diverged author logs",
        )
        .await
    }

    /// The newest committer epoch (seconds) among `oids`.
    async fn newest_commit_epoch(&self, oids: &[&str]) -> Result<i64> {
        let mut newest = 0_i64;
        for oid in oids {
            let raw = trimmed(
                self.git(&["show", "-s", "--format=%ct", oid], None, &[])
                    .await?,
            )?;
            let seconds: i64 = raw.parse().map_err(|e| {
                Error::Substrate(format!("unparsable committer date for {oid}: {e}"))
            })?;
            newest = newest.max(seconds);
        }
        Ok(newest)
    }

    /// Push every local author ref for `channel` to `remote`. A rejection
    /// (the remote advanced since our fetch) is reported as
    /// [`PushOutcome::Rejected`] for the caller to re-cycle; any other failure
    /// is an error.
    async fn push_channel(&self, remote: &str, channel: &ChannelId) -> Result<PushOutcome> {
        let refs = self.channel_refs(channel).await?;
        if refs.is_empty() {
            return Ok(PushOutcome::Done);
        }
        let refspecs: Vec<String> = refs.iter().map(|r| format!("{r}:{r}")).collect();

        let mut args = vec!["push", "--quiet", remote];
        args.extend(refspecs.iter().map(String::as_str));
        let out = self.git_raw(&args, None, &[]).await?;
        if out.status.success() {
            return Ok(PushOutcome::Done);
        }
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        // git reports a stale push as rejected / fetch-first; everything else
        // (no such remote, auth, corruption) is a real fault.
        if stderr.contains("rejected")
            || stderr.contains("fetch first")
            || stderr.contains("failed to push some refs")
        {
            Ok(PushOutcome::Rejected(stderr))
        } else {
            Err(Error::Substrate(format!(
                "git push to {remote} failed: {stderr}"
            )))
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

        let mut last_update_stderr = String::new();
        for _ in 0..MAX_APPEND_ATTEMPTS {
            let tip = self.ref_tip(&refname).await?;

            // Read the author's current log (empty on first write), append the line.
            let mut contents = match &tip {
                Some(_) => self.log_at(&refname).await?,
                None => Vec::new(),
            };
            contents.extend_from_slice(&line);

            let parents: Vec<&str> = tip.as_deref().into_iter().collect();
            let commit = self
                .commit_log(&contents, &parents, &envs, &message)
                .await?;

            // Move the ref, guarded by the value we read — losing the race means
            // a concurrent same-author write landed; re-read and retry.
            let old = tip.as_deref().unwrap_or(ZERO_OID);
            let update = self
                .git_raw(&["update-ref", &refname, &commit, old], None, &[])
                .await?;
            if update.status.success() {
                return Ok(());
            }
            // A failure here is usually the optimistic guard losing a race, but
            // can also be a non-race fault (permissions, corrupt repo) — keep
            // the last stderr so the exhausted-retries error stays honest.
            last_update_stderr = String::from_utf8_lossy(&update.stderr).trim().to_string();
        }

        Err(Error::Substrate(format!(
            "update-ref for {refname} failed after {MAX_APPEND_ATTEMPTS} attempts \
             (a contended ref, or a non-race fault): {last_update_stderr}"
        )))
    }

    async fn entries(&self, channel: &ChannelId) -> Result<Vec<LedgerEntry>> {
        let mut entries = Vec::new();
        for refname in self.channel_refs(channel).await? {
            let blob = self.log_at(&refname).await?;
            for line in blob.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
                entries.push(LedgerEntry::from_canonical_bytes(line)?);
            }
        }
        Ok(entries)
    }

    async fn channels(&self) -> Result<Vec<ChannelId>> {
        // Refnames are refs/junto/<channel-id>/<author-slug>; the channel id
        // is the third path segment. Many author refs per channel → dedupe.
        let out = self
            .git(
                &["for-each-ref", "--format=%(refname)", "refs/junto/"],
                None,
                &[],
            )
            .await?;
        let text = String::from_utf8(out)
            .map_err(|e| Error::Substrate(format!("for-each-ref output not utf-8: {e}")))?;
        let mut seen = std::collections::HashSet::new();
        let mut channels = Vec::new();
        for refname in text.lines().filter(|l| !l.is_empty()) {
            let Some(id_segment) = refname.split('/').nth(2) else {
                continue;
            };
            let Ok(channel) = id_segment.parse::<ChannelId>() else {
                // Tolerate foreign refs under refs/junto/ rather than failing
                // the whole enumeration.
                continue;
            };
            if seen.insert(channel) {
                channels.push(channel);
            }
        }
        Ok(channels)
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
/// The email is **ASCII-lowercased first**: loose refs are files, and on the
/// case-insensitive filesystems junto targets (NTFS, default APFS) two slugs
/// differing only in case would collide as paths while staying distinct on
/// Linux CI — the exact casing trap CLAUDE.md warns about. Emails differing
/// only in case therefore share one ref; that is safe, because the partition
/// exists only to avoid write contention (the optimistic `update-ref` retry
/// already handles same-ref writers) and each entry carries its true-case
/// author email in its JSON.
///
/// We then keep only ASCII lowercase alphanumerics, `-`, and `_`,
/// percent-encoding everything else (incl. `.` and `@`). Dropping `.` is
/// deliberate — it removes any chance of a forbidden `..` sequence or a
/// leading/trailing dot in a ref component.
fn author_slug(email: &str) -> String {
    let email = email.to_ascii_lowercase();
    let mut slug = String::with_capacity(email.len());
    for &byte in email.as_bytes() {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => slug.push(byte as char),
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

    /// A fresh **bare** repo in a tempdir — the stand-in for the forge hub —
    /// plus its path in the string form `sync` takes as a remote.
    fn init_bare_hub() -> (TempDir, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        let status = StdCommand::new("git")
            .args(["init", "--bare", "-q"])
            .current_dir(dir.path())
            .status()
            .expect("run git init --bare");
        assert!(status.success(), "git init --bare failed");
        let url = dir.path().to_string_lossy().into_owned();
        (dir, url)
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
                frame: None,
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

    #[test]
    fn author_slug_is_case_insensitive_and_ref_safe() {
        // Loose refs are files; on case-insensitive filesystems (NTFS, APFS)
        // slugs differing only in case would collide as paths. Lowercasing
        // makes the mapping case-stable on every platform.
        assert_eq!(
            author_slug("Ada@Example.COM"),
            author_slug("ada@example.com")
        );
        assert_eq!(author_slug("ada@example.com"), "ada%40example%2Ecom");
    }

    #[tokio::test]
    async fn case_variant_emails_share_one_ref_without_losing_entries() {
        // "Ada@example.com" and "ada@example.com" map to the same ref; both
        // appends must accumulate (the optimistic update-ref retry handles the
        // shared-ref contention), and the union returns both entries intact.
        let (dir, mut substrate) = init_repo();
        let upper = Member::human("Ada", "Ada@example.com");
        let lower = Member::human("Ada", "ada@example.com");
        let a = assertion(&upper, 10, "from Upper");
        let mut b = assertion(&lower, 20, "from lower");
        b.channel = a.channel;
        substrate.append(a.clone()).await.unwrap();
        substrate.append(b.clone()).await.unwrap();

        let refs = git_out(
            dir.path(),
            &[
                "for-each-ref",
                "--format=%(refname)",
                &format!("refs/junto/{}/", a.channel),
            ],
        );
        assert_eq!(refs.lines().count(), 1, "case variants share one ref");

        let mut got = substrate.entries(&a.channel).await.unwrap();
        got.sort_by_key(|e| e.timestamp.as_millis());
        // True-case emails survive inside the entries themselves.
        assert_eq!(got, vec![a, b]);
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
                frame: None,
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

    // --- sync (forge-as-hub, with a local bare repo as the hub) ---

    #[tokio::test]
    async fn sync_propagates_the_record_between_clones() {
        let (_hub_dir, hub) = init_bare_hub();
        let (_a_dir, mut machine_a) = init_repo();
        let (_b_dir, mut machine_b) = init_repo();
        let ada = Member::human("Ada", "ada@example.com");
        let entry = assertion(&ada, 10, "written on A");
        let channel = entry.channel;

        machine_a.append(entry.clone()).await.unwrap();
        machine_a.sync(&hub, &channel).await.unwrap();
        machine_b.sync(&hub, &channel).await.unwrap();

        assert_eq!(machine_b.entries(&channel).await.unwrap(), vec![entry]);
    }

    #[tokio::test]
    async fn sync_merges_different_authors_across_machines() {
        let (_hub_dir, hub) = init_bare_hub();
        let (_a_dir, mut machine_a) = init_repo();
        let (_b_dir, mut machine_b) = init_repo();
        let ada = Member::human("Ada", "ada@example.com");
        let bob = Member::human("Bob", "bob@example.com");
        let from_ada = assertion(&ada, 10, "from ada on A");
        let mut from_bob = assertion(&bob, 20, "from bob on B");
        from_bob.channel = from_ada.channel;
        let channel = from_ada.channel;

        machine_a.append(from_ada.clone()).await.unwrap();
        machine_b.append(from_bob.clone()).await.unwrap();
        machine_a.sync(&hub, &channel).await.unwrap();
        machine_b.sync(&hub, &channel).await.unwrap();
        machine_a.sync(&hub, &channel).await.unwrap();

        // Both machines project the identical two-entry record.
        let view_a = Ledger::new(machine_a).project(&channel).await.unwrap();
        let view_b = Ledger::new(machine_b).project(&channel).await.unwrap();
        let ids_a: Vec<_> = view_a.entries.iter().map(|e| e.id).collect();
        let ids_b: Vec<_> = view_b.entries.iter().map(|e| e.id).collect();
        assert_eq!(ids_a, vec![from_ada.id, from_bob.id]);
        assert_eq!(ids_a, ids_b);
    }

    #[tokio::test]
    async fn same_author_divergence_unions_without_loss() {
        // The one real conflict shape: the same author appends on two machines
        // while offline, so their author ref diverges. Reconciliation is a
        // union-merge — no entry may be lost, and both machines must converge
        // on the same ref tip.
        let (hub_dir, hub) = init_bare_hub();
        let (a_dir, mut machine_a) = init_repo();
        let (b_dir, mut machine_b) = init_repo();
        let ada = Member::human("Ada", "ada@example.com");
        let on_a = assertion(&ada, 10, "offline on A");
        let mut on_b = assertion(&ada, 20, "offline on B");
        on_b.channel = on_a.channel;
        let channel = on_a.channel;

        machine_a.append(on_a.clone()).await.unwrap();
        machine_b.append(on_b.clone()).await.unwrap();
        machine_a.sync(&hub, &channel).await.unwrap();
        // B's push is rejected (hub holds A's tip), forcing the
        // fetch → union-merge → push cycle.
        machine_b.sync(&hub, &channel).await.unwrap();
        machine_a.sync(&hub, &channel).await.unwrap();

        // No loss: both entries on both machines.
        for substrate in [&machine_a, &machine_b] {
            let mut got = substrate.entries(&channel).await.unwrap();
            got.sort_by_key(|e| e.timestamp.as_millis());
            assert_eq!(got, vec![on_a.clone(), on_b.clone()]);
        }

        // Convergence: one author ref, same tip everywhere (hub included).
        let refname = format!("refs/junto/{channel}/{}", author_slug(&ada.email));
        let tip_a = git_out(a_dir.path(), &["rev-parse", &refname]);
        let tip_b = git_out(b_dir.path(), &["rev-parse", &refname]);
        let tip_hub = git_out(hub_dir.path(), &["rev-parse", &refname]);
        assert_eq!(tip_a, tip_b);
        assert_eq!(tip_a, tip_hub);

        // The merged log is the union in canonical order: exactly two lines.
        let blob = git_out(a_dir.path(), &["show", &format!("{refname}:{LOG_FILE}")]);
        assert_eq!(blob.matches('\n').count(), 2, "two framed lines: {blob}");
    }

    #[tokio::test]
    async fn sync_is_idempotent_once_converged() {
        let (_hub_dir, hub) = init_bare_hub();
        let (a_dir, mut machine_a) = init_repo();
        let ada = Member::human("Ada", "ada@example.com");
        let entry = assertion(&ada, 10, "x");
        let channel = entry.channel;
        machine_a.append(entry).await.unwrap();
        machine_a.sync(&hub, &channel).await.unwrap();

        let refname = format!("refs/junto/{channel}/{}", author_slug(&ada.email));
        let tip_before = git_out(a_dir.path(), &["rev-parse", &refname]);
        machine_a.sync(&hub, &channel).await.unwrap();
        let tip_after = git_out(a_dir.path(), &["rev-parse", &refname]);
        assert_eq!(tip_before, tip_after, "a no-op sync must not mint commits");
    }

    #[tokio::test]
    async fn sync_with_nothing_anywhere_is_a_no_op() {
        let (_hub_dir, hub) = init_bare_hub();
        let (_a_dir, mut machine_a) = init_repo();
        let channel = junto_kernel::ChannelId::new();
        machine_a.sync(&hub, &channel).await.unwrap();
        assert!(machine_a.entries(&channel).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn sync_keeps_the_working_tree_clean() {
        let (_hub_dir, hub) = init_bare_hub();
        let (a_dir, mut machine_a) = init_repo();
        let ada = Member::human("Ada", "ada@example.com");
        let entry = assertion(&ada, 10, "x");
        let channel = entry.channel;
        machine_a.append(entry).await.unwrap();
        machine_a.sync(&hub, &channel).await.unwrap();

        let status = git_out(a_dir.path(), &["status", "--porcelain"]);
        assert!(
            status.trim().is_empty(),
            "working tree must stay clean: {status:?}"
        );
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
