//! Read-only span re-anchoring against a worktree.
//!
//! Everything else in this crate touches only the object DB and refs — this
//! module is the deliberate, sole exception (see the crate-level doc
//! comment): [`reanchor`] answers "where do these pinned lines live now?",
//! and that question can only be asked against whatever is actually checked
//! out. It shells out to `git diff` against a worktree and nothing else —
//! never `add`, `commit`, `checkout`, or any command that could mutate it.
//!
//! The answer is one of three [`Reanchor`] outcomes. `Orphaned` is not a
//! failure mode: it is how the caller (the live-session-plane UI) knows to
//! render a comment against its frozen `CodeAnchor` snapshot instead of
//! pointing at code that no longer corresponds to it.
//!
//! The hunk arithmetic ([`map_span`]) is a pure function over parsed
//! [`Hunk`]s, unit-tested without spawning git at all; `reanchor` is a thin
//! wrapper that gets a unified diff from git, parses it, and hands it to
//! `map_span`.

use std::path::Path;
use std::process::Stdio;

use junto_kernel::{CodeAnchor, Error, Result, Span};
use tokio::process::Command;

/// Where a `CodeAnchor`'s pinned span sits in a worktree now, relative to
/// where it was pinned at the anchor's commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reanchor {
    /// No hunk shifted or touched the span: the same line numbers still name
    /// the same lines (other parts of the file may have changed).
    Exact {
        /// The span, unchanged.
        span: Span,
    },
    /// Lines above the span were inserted or removed, shifting it to a new
    /// line range; the span's own lines were not directly edited.
    Moved {
        /// The span's new line numbers.
        span: Span,
    },
    /// The pinned lines were directly edited or deleted (or the file itself
    /// was deleted), so no line-number span can be trusted to still name
    /// them. Not a failure: the caller falls back to the anchor's frozen
    /// snapshot.
    Orphaned,
}

/// One `@@ -old_start,old_len +new_start,new_len @@` hunk header from a
/// unified diff, as `git diff --unified=0` emits it. `--unified=0` means
/// every hunk header carries exactly one contiguous change; there are no
/// surrounding context lines to also parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Hunk {
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
}

/// Parse every hunk header out of a unified diff. `git` omits the `,len`
/// suffix when a range is exactly one line (`@@ -10 +14 @@` means
/// `old_len`/`new_len` of `1`); non-header lines (file headers, `+`/`-`
/// content lines) are ignored.
pub(crate) fn parse_hunks(diff: &str) -> Vec<Hunk> {
    diff.lines().filter_map(parse_hunk_header).collect()
}

/// Parse one `@@ -a[,b] +c[,d] @@[ trailing context]` line, or `None` if it
/// is not a hunk header.
fn parse_hunk_header(line: &str) -> Option<Hunk> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, _trailing) = rest.split_once(" @@")?;
    let (old_start, old_len) = parse_range(old)?;
    let (new_start, new_len) = parse_range(new)?;
    Some(Hunk {
        old_start,
        old_len,
        new_start,
        new_len,
    })
}

/// Parse one `start[,len]` half of a hunk header, defaulting `len` to `1`
/// when git omits it (a single-line range).
fn parse_range(range: &str) -> Option<(u32, u32)> {
    match range.split_once(',') {
        Some((start, len)) => Some((start.parse().ok()?, len.parse().ok()?)),
        None => Some((range.parse().ok()?, 1)),
    }
}

/// Map `span` through `hunks` (pure — no I/O): the hunk arithmetic behind
/// `reanchor`.
///
/// A hunk whose old range `[old_start, old_start + old_len)` intersects
/// `span` orphans it (the pinned lines were directly touched). A pure
/// insertion (`old_len == 0`) never intersects anything — by construction it
/// consumes no old line — so inserting exactly at `span.start` only shifts
/// what follows rather than orphaning it. Otherwise, sum `new_len - old_len`
/// over every hunk that lies entirely before `span.start` (`old_end <=
/// span.start`) and shift the span by that total; a total of zero is
/// `Exact`, anything else is `Moved`.
pub(crate) fn map_span(span: Span, hunks: &[Hunk]) -> Reanchor {
    let mut shift: i64 = 0;
    for hunk in hunks {
        let old_end = hunk.old_start + hunk.old_len; // exclusive upper bound
        let overlaps_span = hunk.old_len > 0 && hunk.old_start <= span.end && span.start < old_end;
        if overlaps_span {
            return Reanchor::Orphaned;
        }
        if old_end <= span.start {
            shift += i64::from(hunk.new_len) - i64::from(hunk.old_len);
        }
    }
    if shift == 0 {
        return Reanchor::Exact { span };
    }
    shift_span(span, shift).map_or(Reanchor::Orphaned, |span| Reanchor::Moved { span })
}

/// Apply a net line-count `shift` to `span`, re-validating through
/// [`Span::new`]. `None` only if the shift would push the span out of range
/// — not reachable from a real unified diff (hunks partition disjoint,
/// non-overlapping old-file ranges, so the total shift from hunks entirely
/// above `span.start` can never remove more than `span.start - 1` lines);
/// kept so this pure function can never panic on out-of-range arithmetic.
fn shift_span(span: Span, shift: i64) -> Option<Span> {
    let start = u32::try_from(i64::from(span.start) + shift).ok()?;
    let end = u32::try_from(i64::from(span.end) + shift).ok()?;
    Span::new(start, end).ok()
}

/// Run `git -C <dir> <args>`, requiring a zero exit; returns stdout or an
/// error carrying stderr.
///
/// A free function, not a method: this module has no substrate/state to hang
/// it off, just a worktree path per call. Mirrors
/// [`crate::GitRefsSubstrate::git_raw`]'s spawn shape — including the
/// Windows `CREATE_NO_WINDOW` guard — so this, the crate's one
/// working-tree-touching path, stays just as console-silent as every
/// ref/object-DB call the rest of the crate makes.
async fn git_in(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    // Same chokepoint reasoning as `GitRefsSubstrate::git_raw`: every spawn
    // on Windows would otherwise flash a console window.
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

    let out = cmd
        .output()
        .await
        .map_err(|e| Error::Substrate(format!("could not run git: {e}")))?;
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

/// Where `anchor`'s pinned span sits in `worktree` right now.
///
/// Read-only: runs `git diff --unified=0 <anchor.commit> -- <anchor.path>`
/// against `worktree` and nothing else. This is the one place in the crate
/// that reads a working tree at all (see the module doc comment) — every
/// other substrate operation in this crate touches only the object DB and
/// refs, never a working tree.
///
/// Does not compare `anchor.blob`: this function answers *position* only —
/// content-drift detection against the pinned blob is a later slice.
///
/// # Errors
/// Returns [`Error::Substrate`] if `git` could not be run (not a git repo,
/// unknown commit, `git` missing from `PATH`, …) or its output was not
/// UTF-8.
pub async fn reanchor(worktree: &Path, anchor: &CodeAnchor) -> Result<Reanchor> {
    let diff = git_in(
        worktree,
        &[
            "diff",
            "--unified=0",
            anchor.commit.as_str(),
            "--",
            &anchor.path,
        ],
    )
    .await?;
    let diff = String::from_utf8(diff)
        .map_err(|e| Error::Substrate(format!("git diff output was not utf-8: {e}")))?;

    if diff.is_empty() {
        return Ok(Reanchor::Exact { span: anchor.span });
    }
    if diff.contains("deleted file mode") || diff.contains("+++ /dev/null") {
        return Ok(Reanchor::Orphaned);
    }
    Ok(map_span(anchor.span, &parse_hunks(&diff)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use junto_kernel::{CommitOid, ContentDigest};
    use std::path::Path;
    use std::process::Command as StdCommand;

    // ---- Step 1: pure hunk-math tests ----

    #[test]
    fn parse_hunks_reads_headers_with_and_without_lengths() {
        let diff = "@@ -3,2 +5,4 @@\n@@ -10 +14 @@\n";
        let hunks = parse_hunks(diff);
        assert_eq!(
            hunks[0],
            Hunk {
                old_start: 3,
                old_len: 2,
                new_start: 5,
                new_len: 4
            }
        );
        assert_eq!(
            hunks[1],
            Hunk {
                old_start: 10,
                old_len: 1,
                new_start: 14,
                new_len: 1
            }
        );
    }

    #[test]
    fn untouched_file_is_exact() {
        let span = Span::new(5, 8).unwrap();
        assert_eq!(map_span(span, &[]), Reanchor::Exact { span });
    }

    #[test]
    fn insertion_above_moves_span() {
        // 3 lines inserted at old line 2 → span shifts down by 3.
        let hunks = [Hunk {
            old_start: 2,
            old_len: 0,
            new_start: 2,
            new_len: 3,
        }];
        assert_eq!(
            map_span(Span::new(5, 8).unwrap(), &hunks),
            Reanchor::Moved {
                span: Span::new(8, 11).unwrap()
            }
        );
    }

    #[test]
    fn edit_inside_span_orphans() {
        let hunks = [Hunk {
            old_start: 6,
            old_len: 1,
            new_start: 6,
            new_len: 1,
        }];
        assert_eq!(
            map_span(Span::new(5, 8).unwrap(), &hunks),
            Reanchor::Orphaned
        );
    }

    #[test]
    fn deletion_below_span_is_exact() {
        let hunks = [Hunk {
            old_start: 20,
            old_len: 4,
            new_start: 20,
            new_len: 0,
        }];
        let span = Span::new(5, 8).unwrap();
        assert_eq!(map_span(span, &hunks), Reanchor::Exact { span });
    }

    // ---- Step 4: scripted-repo tests against real git ----

    fn run_git(repo: &Path, args: &[&str]) {
        let status = StdCommand::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    fn git_stdout(repo: &Path, args: &[&str]) -> Vec<u8> {
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
        out.stdout
    }

    #[tokio::test]
    async fn reanchor_end_to_end_states() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        run_git(repo, &["init", "-q"]);
        std::fs::write(repo.join("f.txt"), "a\nb\nc\nd\ne\n").unwrap();
        run_git(repo, &["add", "."]);
        run_git(
            repo,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "base",
            ],
        );
        let commit = String::from_utf8(git_stdout(repo, &["rev-parse", "HEAD"]))
            .unwrap()
            .trim()
            .to_string();
        let anchor = CodeAnchor {
            commit: CommitOid::new(commit).unwrap(),
            path: "f.txt".into(),
            blob: ContentDigest::new("sha256:unused-here").unwrap(),
            span: Span::new(3, 4).unwrap(), // lines "c","d"
        };
        // Unchanged → Exact
        assert!(matches!(
            reanchor(repo, &anchor).await.unwrap(),
            Reanchor::Exact { .. }
        ));
        // Insert two lines at top → Moved to 5..6
        std::fs::write(repo.join("f.txt"), "x\ny\na\nb\nc\nd\ne\n").unwrap();
        assert_eq!(
            reanchor(repo, &anchor).await.unwrap(),
            Reanchor::Moved {
                span: Span::new(5, 6).unwrap()
            }
        );
        // Edit inside the span → Orphaned
        std::fs::write(repo.join("f.txt"), "a\nb\nZZZ\nd\ne\n").unwrap();
        assert_eq!(reanchor(repo, &anchor).await.unwrap(), Reanchor::Orphaned);
        // Delete the file → Orphaned
        std::fs::remove_file(repo.join("f.txt")).unwrap();
        assert_eq!(reanchor(repo, &anchor).await.unwrap(), Reanchor::Orphaned);
    }
}
