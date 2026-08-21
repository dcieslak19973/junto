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
/// Git's hunk header `@@ -a,b +c,d @@` means "old lines `[a, a+b)` became
/// `d` new lines starting at `c`". The classification differs by whether the
/// hunk is a pure insertion:
///
/// - **Insertion** (`old_len == 0`, i.e. `b == 0`): the old range is empty —
///   git's convention is that the new lines land immediately *after* old
///   line `a` (`a == 0` means "before old line 1"). So an insertion strictly
///   above the span (`a < span.start`) only shifts what follows; one landing
///   on or after the span's last line (`a >= span.end`) does not touch the
///   span at all — in particular, a single-line span (`start == end`) with
///   an insertion directly below it stays `Exact`, because the pinned line
///   genuinely did not move. Anything in between (`span.start <= a <
///   span.end`) inserts new lines *inside* the span and orphans it.
/// - **Modification/deletion** (`old_len > 0`): the hunk actually consumes
///   old lines `[a, a+old_len)`. One that intersects `[span.start,
///   span.end]` orphans the span (its lines were directly touched); one
///   entirely before the span (`a + old_len <= span.start`) only shifts it.
///
/// Either way, sum `new_len - old_len` over every hunk classified as "above"
/// and shift the span by that total: zero is `Exact`, anything else is
/// `Moved`.
pub(crate) fn map_span(span: Span, hunks: &[Hunk]) -> Reanchor {
    let mut shift: i64 = 0;
    for hunk in hunks {
        let a = hunk.old_start;
        if hunk.old_len == 0 {
            if a < span.start {
                shift += i64::from(hunk.new_len);
            } else if a < span.end {
                return Reanchor::Orphaned;
            }
            // else `a >= span.end`: the insertion lands at or after the
            // span's last line — below it, no effect.
        } else {
            let old_end = a + hunk.old_len; // exclusive upper bound
            if old_end <= span.start {
                shift += i64::from(hunk.new_len) - i64::from(hunk.old_len);
            } else if a <= span.end {
                return Reanchor::Orphaned;
            }
            // else `a > span.end`: the hunk is entirely below the span, no
            // effect.
        }
    }
    if shift == 0 {
        return Reanchor::Exact { span };
    }
    shift_span(span, shift).map_or(Reanchor::Orphaned, |span| Reanchor::Moved { span })
}

/// Apply a net line-count `shift` to `span`, re-validating through
/// [`Span::new`]. `None` only if the shift would push the span's start below
/// line 1 — not reachable from a hunk list a real unified diff would ever
/// produce (old-file hunk ranges start at line 1 or later, so a shift from
/// hunks entirely above `span.start` can never remove more than `span.start
/// - 1` lines); kept, and unit-tested below, so this pure function degrades
/// to `Orphaned` instead of panicking if ever handed a malformed `Hunk` list.
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
/// Read-only: runs `git diff --unified=0 <anchor.commit> -- <pathspec>`
/// against `worktree` and nothing else. This is the one place in the crate
/// that reads a working tree at all (see the module doc comment) — every
/// other substrate operation in this crate touches only the object DB and
/// refs, never a working tree.
///
/// The pathspec is built as `:(top,literal)<anchor.path>`: `literal`
/// disables fnmatch/glob interpretation of `anchor.path` (a path like a
/// framework's `app/[id]/page.tsx` route segment must not be read as a
/// one-character glob class), and `top` re-anchors it to the repo root —
/// `anchor.path` is documented repo-relative, but a bare pathspec resolves
/// relative to `-C worktree`, which silently matches nothing when
/// `worktree` is a subdirectory of the repo rather than its root.
///
/// `--no-ext-diff --no-textconv` force git's own unified-diff algorithm and
/// raw content compare, bypassing any configured `diff.external` (e.g.
/// difftastic) or attribute-driven `textconv` driver — either would replace
/// the parseable unified output this function depends on, potentially
/// hiding a real change. Proportionate to this crate's existing
/// `--no-filters` guard in [`crate::GitRefsSubstrate::git_raw`] for the same
/// hazard class.
///
/// Does not compare `anchor.blob`: this function answers *position* only —
/// content-drift detection against the pinned blob is a later slice.
///
/// # Errors
/// Returns [`Error::Substrate`] if `git` could not be run (not a git repo,
/// unknown commit, `git` missing from `PATH`, …) or its output was not
/// UTF-8.
pub async fn reanchor(worktree: &Path, anchor: &CodeAnchor) -> Result<Reanchor> {
    let pathspec = format!(":(top,literal){}", anchor.path);
    let diff = git_in(
        worktree,
        &[
            "diff",
            "--unified=0",
            "--no-ext-diff",
            "--no-textconv",
            anchor.commit.as_str(),
            "--",
            &pathspec,
        ],
    )
    .await?;
    let diff = String::from_utf8(diff)
        .map_err(|e| Error::Substrate(format!("git diff output was not utf-8: {e}")))?;

    if diff.is_empty() {
        return Ok(Reanchor::Exact { span: anchor.span });
    }
    if is_deleted_file_diff(&diff) {
        return Ok(Reanchor::Orphaned);
    }
    Ok(map_span(anchor.span, &parse_hunks(&diff)))
}

/// Whether a unified diff represents a deleted file: `deleted file mode` or
/// `+++ /dev/null` in the **file header** — the lines before the first hunk
/// header. Scanning the whole diff would false-positive on a file whose
/// *content* happens to contain those strings as `+`-prefixed added lines
/// (e.g. this repo's own patch/diff fixtures): an added content line reading
/// literally `+++ /dev/null` renders in the diff as `++++ /dev/null`, which
/// still contains the substring.
fn is_deleted_file_diff(diff: &str) -> bool {
    let header_end = diff.find("\n@@ -").unwrap_or(diff.len());
    let header = &diff[..header_end];
    header.contains("deleted file mode") || header.contains("+++ /dev/null")
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

    #[test]
    fn insertion_immediately_below_single_line_span_is_exact() {
        // Ruling example 1: span 3..=3, `@@ -3,0 +4 @@` — the pinned line
        // never moved, only what comes after it did.
        let hunks = [Hunk {
            old_start: 3,
            old_len: 0,
            new_start: 4,
            new_len: 1,
        }];
        let span = Span::new(3, 3).unwrap();
        assert_eq!(map_span(span, &hunks), Reanchor::Exact { span });
    }

    #[test]
    fn insertion_strictly_inside_span_orphans() {
        // Ruling example 2: span 3..=5, `@@ -4,0 +5 @@` — new lines land
        // between the span's first and last line, so it no longer names a
        // single contiguous unedited block.
        let hunks = [Hunk {
            old_start: 4,
            old_len: 0,
            new_start: 5,
            new_len: 1,
        }];
        assert_eq!(
            map_span(Span::new(3, 5).unwrap(), &hunks),
            Reanchor::Orphaned
        );
    }

    #[test]
    fn insertion_at_span_start_of_multiline_span_orphans() {
        // Landing exactly at the span's first line still inserts inside it.
        let hunks = [Hunk {
            old_start: 3,
            old_len: 0,
            new_start: 4,
            new_len: 2,
        }];
        assert_eq!(
            map_span(Span::new(3, 5).unwrap(), &hunks),
            Reanchor::Orphaned
        );
    }

    #[test]
    fn insertion_at_span_end_of_multiline_span_is_exact() {
        // Landing at the span's last line inserts *after* it, not inside.
        let hunks = [Hunk {
            old_start: 4,
            old_len: 0,
            new_start: 5,
            new_len: 2,
        }];
        let span = Span::new(3, 4).unwrap();
        assert_eq!(map_span(span, &hunks), Reanchor::Exact { span });
    }

    #[test]
    fn deletion_ending_exactly_at_span_start_shifts() {
        // old_end == span.start is the "above" boundary, inclusive.
        let hunks = [Hunk {
            old_start: 1,
            old_len: 2,
            new_start: 1,
            new_len: 0,
        }];
        assert_eq!(
            map_span(Span::new(3, 5).unwrap(), &hunks),
            Reanchor::Moved {
                span: Span::new(1, 3).unwrap()
            }
        );
    }

    #[test]
    fn edit_starting_exactly_at_span_end_orphans() {
        // A hunk that starts on the span's last line touches it even though
        // it doesn't reach past `span.end`.
        let hunks = [Hunk {
            old_start: 5,
            old_len: 1,
            new_start: 5,
            new_len: 1,
        }];
        assert_eq!(
            map_span(Span::new(3, 5).unwrap(), &hunks),
            Reanchor::Orphaned
        );
    }

    #[test]
    fn hunk_straddling_span_top_edge_orphans() {
        // Old range [4,7) starts before the span but reaches into it.
        let hunks = [Hunk {
            old_start: 4,
            old_len: 3,
            new_start: 4,
            new_len: 3,
        }];
        assert_eq!(
            map_span(Span::new(5, 8).unwrap(), &hunks),
            Reanchor::Orphaned
        );
    }

    #[test]
    fn multiple_hunks_above_accumulate_shift() {
        let hunks = [
            Hunk {
                old_start: 1,
                old_len: 1,
                new_start: 1,
                new_len: 4,
            }, // net +3
            Hunk {
                old_start: 5,
                old_len: 2,
                new_start: 8,
                new_len: 1,
            }, // net -1
        ];
        assert_eq!(
            map_span(Span::new(10, 12).unwrap(), &hunks),
            Reanchor::Moved {
                span: Span::new(12, 14).unwrap()
            }
        );
    }

    #[test]
    fn malformed_hunk_list_that_would_underflow_orphans_instead_of_panicking() {
        // No real diff emits old_start == 0 with old_len > 0 (that combination
        // is git's convention for "before line 1", used only for pure
        // insertions, old_len == 0). Feeding `map_span` a hand-built `Hunk`
        // shaped like that anyway must degrade to `Orphaned`, not panic —
        // exercises `shift_span`'s `u32::try_from`/`None` fallback.
        let hunks = [Hunk {
            old_start: 0,
            old_len: 3,
            new_start: 0,
            new_len: 0,
        }];
        assert_eq!(
            map_span(Span::new(3, 5).unwrap(), &hunks),
            Reanchor::Orphaned
        );
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

    #[tokio::test]
    async fn reanchor_from_a_subdirectory_worktree_finds_a_bracket_path() {
        // Two compounding hazards from a raw (non-literal, non-anchored)
        // pathspec: (1) `[id]` is fnmatch/glob syntax for a one-character
        // class, which `core.globPathspecs` can turn on; (2) `CodeAnchor`'s
        // `path` is documented repo-relative (junto-kernel's `anchor`
        // module), but a bare pathspec resolves relative to `-C`'s
        // directory — so calling `reanchor` with a worktree that is a
        // *subdirectory* of the repo (a legitimate call shape) silently
        // matches nothing. `:(top,literal)` fixes both: `literal` disables
        // glob interpretation, `top` re-anchors to the repo root regardless
        // of `-C`'s cwd.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        run_git(repo, &["init", "-q"]);
        std::fs::create_dir_all(repo.join("app").join("[id]")).unwrap();
        let rel_path = "app/[id]/page.tsx";
        std::fs::write(repo.join(rel_path), "a\nb\nc\n").unwrap();
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
            path: rel_path.into(),
            blob: ContentDigest::new("sha256:unused-here").unwrap(),
            span: Span::new(2, 2).unwrap(),
        };
        // Edit the span's own line, but call `reanchor` with `worktree`
        // pointed at the `app` subdirectory rather than the repo root — a
        // caller that resolved a worktree path some other way could
        // legitimately do this. `anchor.path` stays repo-relative.
        std::fs::write(repo.join(rel_path), "a\nZZZ\nc\n").unwrap();
        let subdir_worktree = repo.join("app");
        assert_eq!(
            reanchor(&subdir_worktree, &anchor).await.unwrap(),
            Reanchor::Orphaned
        );
    }

    #[tokio::test]
    async fn diff_external_configured_does_not_hide_the_change() {
        // A configured `diff.external` (e.g. difftastic) replaces git's
        // unified-diff output with the tool's own — here, an external
        // driver that always claims "no differences". `--no-ext-diff`
        // is proportionate hardening: this crate already guards the same
        // hazard class for `--no-filters` (see `lib.rs`'s `git_raw` and the
        // `core_autocrlf` test).
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        run_git(repo, &["init", "-q"]);
        std::fs::write(repo.join("f.txt"), "a\nb\nc\n").unwrap();
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
        std::fs::write(repo.join("noop-diff.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        run_git(repo, &["config", "diff.external", "sh ./noop-diff.sh"]);
        let anchor = CodeAnchor {
            commit: CommitOid::new(commit).unwrap(),
            path: "f.txt".into(),
            blob: ContentDigest::new("sha256:unused-here").unwrap(),
            span: Span::new(2, 2).unwrap(),
        };
        std::fs::write(repo.join("f.txt"), "a\nZZZ\nc\n").unwrap(); // edit inside the span
        assert_eq!(reanchor(repo, &anchor).await.unwrap(), Reanchor::Orphaned);
    }

    #[tokio::test]
    async fn diff_textconv_configured_does_not_hide_the_change() {
        // A `diff=driver` gitattribute plus a configured `textconv` swaps in
        // a converted view for the diff — here, a driver that always emits
        // fixed dummy text. `--no-textconv` forces the raw content compare.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        run_git(repo, &["init", "-q"]);
        std::fs::write(repo.join("f.txt"), "a\nb\nc\n").unwrap();
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
        std::fs::write(repo.join(".gitattributes"), "f.txt diff=notext\n").unwrap();
        std::fs::write(repo.join("notext.sh"), "#!/bin/sh\necho fixed\n").unwrap();
        run_git(repo, &["config", "diff.notext.textconv", "sh ./notext.sh"]);
        let anchor = CodeAnchor {
            commit: CommitOid::new(commit).unwrap(),
            path: "f.txt".into(),
            blob: ContentDigest::new("sha256:unused-here").unwrap(),
            span: Span::new(2, 2).unwrap(),
        };
        std::fs::write(repo.join("f.txt"), "a\nZZZ\nc\n").unwrap(); // edit inside the span
        assert_eq!(reanchor(repo, &anchor).await.unwrap(), Reanchor::Orphaned);
    }

    #[tokio::test]
    async fn deletion_markers_in_file_content_do_not_orphan() {
        // A file whose own tracked content contains the literal strings
        // `+++ /dev/null` / `deleted file mode` (e.g. this repo's own patch
        // fixtures) must not be mistaken for an actually-deleted file just
        // because those strings appear, as *added content*, after the first
        // hunk header.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        run_git(repo, &["init", "-q"]);
        std::fs::write(repo.join("patch.txt"), "a\nb\nc\n").unwrap();
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
            path: "patch.txt".into(),
            blob: ContentDigest::new("sha256:unused-here").unwrap(),
            span: Span::new(1, 1).unwrap(),
        };
        // Append lines whose *content* is exactly those marker strings,
        // well below the span — an unrelated, non-deleting edit.
        std::fs::write(
            repo.join("patch.txt"),
            "a\nb\nc\n+++ /dev/null\ndeleted file mode 100644\n",
        )
        .unwrap();
        assert!(matches!(
            reanchor(repo, &anchor).await.unwrap(),
            Reanchor::Exact { .. }
        ));
    }
}
