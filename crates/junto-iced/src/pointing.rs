//! Pointing — the pure logic behind clicking a rendered line to anchor a comment.
//!
//! junto's anchors are line- and block-granular by design:
//! [`junto_kernel::Span`] is "a 1-indexed, inclusive line span within a file —
//! editor line numbers, not byte offsets, since that is what a human reviewer
//! points at", and `StreamAnchor { session, op }` is a block identity. So a
//! pointing gesture needs *click targets*, not selectable text — character
//! selection is what you need to copy prose, not to point. (Iced has no
//! selectable static text, `iced-rs/iced#36`; that gap does not block this.)
//!
//! Everything here is deliberately **renderer-neutral**: no `iced` imports, no
//! I/O, no widget types. The gesture's three decisions live here so they can be
//! unit-tested without a running app — and so they survive intact if junto's
//! desktop surface is ever rebuilt on another toolkit.

use junto_kernel::Span;

/// The new-file location a rendered unified-diff row points at: `Some((path,
/// line))` for a row that occupies a line of the *new* file, `None` for
/// anything a reviewer cannot anchor to.
pub type RowTarget<'a> = Option<(&'a str, u32)>;

/// A plain rectangle, so the popover's placement maths stays renderer-neutral
/// and testable without constructing an `iced::Rectangle`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Where a floating panel of `panel` (width, height) should sit so that it hangs
/// off `anchor` without leaving `viewport` (width, height).
///
/// Below the anchor by default; flipped ABOVE when there is no room below, so a
/// row near the bottom of a pane still gets a usable panel. Horizontally
/// left-aligned with the anchor, then pulled back inside the right edge.
#[must_use]
pub fn popover_position(
    anchor: Rect,
    panel: (f32, f32),
    viewport: (f32, f32),
    gap: f32,
) -> (f32, f32) {
    let (panel_w, panel_h) = panel;
    let (viewport_w, viewport_h) = viewport;
    let below = anchor.y + anchor.height + gap;
    let y = if below + panel_h <= viewport_h {
        below
    } else {
        (anchor.y - panel_h - gap).max(0.0)
    };
    let x = anchor.x.min((viewport_w - panel_w - 8.0).max(0.0)).max(8.0);
    (x, y)
}

/// The new-file line a floating comment panel should hang from: the aimed
/// span's END — the row most recently clicked — but only when one of `diffs`
/// actually renders that row within its first `max_rows` lines.
///
/// The "actually renders" half is load-bearing: the caller hides its fallback
/// composer whenever this returns `Some`, so promising a panel with no anchor
/// would leave the reviewer no way to comment at all.
#[must_use]
pub fn popup_anchor_line<'a>(
    path: &str,
    span: Option<Span>,
    diffs: impl Iterator<Item = &'a str>,
    max_rows: usize,
) -> Option<u32> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    let span = span?;
    let mut diffs = diffs;
    let rendered = diffs.any(|body| {
        diff_row_targets(body)
            .iter()
            .take(max_rows)
            .flatten()
            .any(|(row_path, line)| *row_path == path && *line == span.end)
    });
    rendered.then_some(span.end)
}

#[cfg(test)]
mod placement_tests {
    use super::{Rect, popover_position, popup_anchor_line};
    use junto_kernel::Span;

    const ANCHOR: Rect = Rect {
        x: 40.0,
        y: 100.0,
        width: 300.0,
        height: 16.0,
    };

    #[test]
    fn a_panel_hangs_just_below_its_row() {
        let (x, y) = popover_position(ANCHOR, (500.0, 120.0), (1000.0, 800.0), 4.0);
        assert_eq!(x, 40.0, "left-aligned with the row");
        assert_eq!(y, 120.0, "anchor bottom (116) plus the gap");
    }

    #[test]
    fn a_panel_flips_above_when_there_is_no_room_below() {
        // A row near the bottom of the pane must still get a usable panel
        // rather than one clipped off the edge.
        let anchor = Rect { y: 700.0, ..ANCHOR };
        let (_, y) = popover_position(anchor, (500.0, 120.0), (1000.0, 800.0), 4.0);
        assert_eq!(y, 576.0, "700 - 120 - 4");
    }

    #[test]
    fn a_flipped_panel_taller_than_the_space_above_is_clamped_not_negative() {
        let anchor = Rect { y: 30.0, ..ANCHOR };
        let (_, y) = popover_position(anchor, (500.0, 400.0), (1000.0, 200.0), 4.0);
        assert_eq!(y, 0.0, "never positioned off the top of the viewport");
    }

    #[test]
    fn a_panel_is_pulled_back_inside_the_right_edge() {
        let anchor = Rect { x: 900.0, ..ANCHOR };
        let (x, _) = popover_position(anchor, (500.0, 120.0), (1000.0, 800.0), 4.0);
        assert_eq!(x, 492.0, "1000 - 500 - 8");
    }

    #[test]
    fn a_panel_wider_than_the_viewport_still_starts_on_screen() {
        let (x, _) = popover_position(ANCHOR, (2000.0, 120.0), (600.0, 800.0), 4.0);
        assert_eq!(x, 8.0, "clamped to the left margin, never negative");
    }

    const DIFF: &str = "\
diff --git a/lib.rs b/lib.rs
+++ b/lib.rs
@@ -1,3 +1,4 @@
 fn one() {}
+fn two() {}
 fn three() {}
";

    #[test]
    fn the_anchor_is_the_span_end_when_that_row_is_rendered() {
        let span = Span::new(1, 3).ok();
        assert_eq!(
            popup_anchor_line("lib.rs", span, [DIFF].into_iter(), 500),
            Some(3),
            "the panel hangs off the row most recently clicked"
        );
    }

    #[test]
    fn no_anchor_when_the_row_is_not_rendered_anywhere() {
        // Hiding the fallback composer for a panel that cannot appear would
        // leave no way to comment at all.
        let span = Span::new(90, 99).ok();
        assert_eq!(
            popup_anchor_line("lib.rs", span, [DIFF].into_iter(), 500),
            None
        );
        assert_eq!(
            popup_anchor_line("other.rs", Span::new(1, 1).ok(), [DIFF].into_iter(), 500),
            None,
            "a path no rendered diff mentions anchors nothing"
        );
    }

    #[test]
    fn no_anchor_past_the_render_cut_off() {
        // `max_rows` mirrors the renderer's own truncation, so a row the
        // renderer drops can never be promised a panel.
        assert_eq!(
            popup_anchor_line("lib.rs", Span::new(1, 3).ok(), [DIFF].into_iter(), 4),
            None
        );
    }

    #[test]
    fn no_anchor_without_a_path_or_a_parseable_span() {
        assert_eq!(
            popup_anchor_line("", Span::new(1, 3).ok(), [DIFF].into_iter(), 500),
            None
        );
        assert_eq!(
            popup_anchor_line("lib.rs", None, [DIFF].into_iter(), 500),
            None
        );
    }

    #[test]
    fn the_anchor_is_found_across_several_expanded_diffs() {
        let other = "+++ b/notes.md\n@@ -1,1 +7,1 @@\n+golf\n";
        assert_eq!(
            popup_anchor_line(
                "notes.md",
                Span::new(7, 7).ok(),
                [DIFF, other].into_iter(),
                500
            ),
            Some(7)
        );
    }
}

/// Map every rendered row of a unified diff to the new-file location it shows,
/// one entry per [`str::lines`] row and in the same order — so a renderer that
/// draws the diff row by row can zip the two and make exactly the anchorable
/// rows clickable.
///
/// Anchorable: context rows and `+` rows, which each occupy a line of the new
/// file. Not anchorable (`None`): every header (`diff --git`, `index`, `---`,
/// `+++`, mode/rename lines), the `@@` hunk marker, the `\ No newline` marker,
/// and every `-` row — **a removed line has no new-file line number**, and a
/// `CodeAnchor`'s commit is the new one, so there is nothing there to point at.
///
/// The walk is a single pass over `+++ b/<path>` (which names the new file and
/// resets the counter) and `@@ -a,b +c,d @@` (which restarts the counter at
/// `c`). `+++ /dev/null` (a deletion) leaves no path, so its hunks are
/// unanchorable rather than anchored to nothing.
///
/// A row that is not unified-diff syntax ends the hunk, so anything a producer
/// appends after the last hunk is unanchorable. junto's own diff artifacts
/// need this: they carry an untracked-file trailer.
#[must_use]
pub fn diff_row_targets(diff: &str) -> Vec<RowTarget<'_>> {
    let mut out = Vec::new();
    // The new file every following hunk counts lines in, and the next new-file
    // line number. `cursor: None` means "not inside a hunk" — header territory,
    // where nothing is anchorable.
    let mut path: Option<&str> = None;
    let mut cursor: Option<u32> = None;

    for line in diff.lines() {
        // A new file's header starts here: drop the previous file's path and
        // counter, so its trailing `index`/mode/rename rows cannot be mistaken
        // for context lines of the file that just ended.
        if line.starts_with("diff ") {
            path = None;
            cursor = None;
            out.push(None);
            continue;
        }
        // `+++ b/<path>` names the new file. Checked before the `+` arm below,
        // whose prefix it shares.
        if let Some(rest) = line.strip_prefix("+++ ") {
            path = new_file_path(rest);
            cursor = None;
            out.push(None);
            continue;
        }
        // `@@ -a,b +c,d @@` restarts the new-file counter at `c`.
        if line.starts_with("@@") {
            cursor = hunk_new_start(line);
            out.push(None);
            continue;
        }
        // Outside a hunk, or with no new-side path, nothing is anchorable.
        let (Some(file), Some(line_no)) = (path, cursor) else {
            out.push(None);
            continue;
        };
        // Inside a hunk, unified-diff grammar allows exactly four kinds of
        // row, and anything else means the hunk has ENDED. Being strict here
        // is what stops trailing prose from being counted as code: junto's
        // own diff artifacts append an untracked-file trailer
        // (`# untracked files:` / `?? path`) after the last hunk, and a
        // lenient "treat the unknown as context" rule silently numbered those
        // rows as lines of the previous file.
        match line.as_bytes().first() {
            // Added or context — each occupies one line of the new file.
            Some(b'+' | b' ') => {
                out.push(Some((file, line_no)));
                cursor = Some(line_no + 1);
            }
            // A removed line has no new-file number, and `\ No newline at end
            // of file` annotates the row above: both stay inside the hunk
            // without consuming a new-file line. `-` also absorbs
            // `--- a/<path>` in a bare `diff -u` stream, whose `+++` resets
            // path and cursor immediately after.
            Some(b'-' | b'\\') => out.push(None),
            // Not hunk syntax at all (a blank separator, a trailer, any
            // decoration a producer appends). End the hunk rather than guess:
            // an unanchorable row is a click that does nothing, whereas a
            // mis-numbered one puts the wrong line in a SIGNED annotation.
            _ => {
                cursor = None;
                out.push(None);
            }
        }
    }
    out
}

/// The path a `+++ ` header names, or `None` for `/dev/null` — a deletion,
/// which leaves no new-side line to point at.
fn new_file_path(rest: &str) -> Option<&str> {
    // git writes `+++ b/<path>`; a plain `diff -u` writes `+++ <path>` and may
    // append a tab-separated timestamp.
    let rest = rest.split('\t').next().unwrap_or(rest).trim_end();
    if rest == "/dev/null" || rest.is_empty() {
        return None;
    }
    Some(rest.strip_prefix("b/").unwrap_or(rest))
}

/// The new-file start line of a `@@ -a,b +c,d @@` header (`c`).
///
/// `None` when it does not parse, or for a `+0` hunk (an empty new side):
/// those rows become unanchorable rather than mis-numbered, because a wrong
/// line number in a *signed* annotation points a reviewer at the wrong code.
fn hunk_new_start(line: &str) -> Option<u32> {
    let plus = line.split_whitespace().find(|tok| tok.starts_with('+'))?;
    let start: u32 = plus[1..].split(',').next()?.parse().ok()?;
    (start > 0).then_some(start)
}

/// What the composer's `path` and `lines` inputs become after clicking the diff
/// row for `line` of `path`, given what they hold now.
///
/// A second click further down the same file **extends** the range — `"12"`
/// then row 14 gives `"12-14"`, which is exactly the form
/// `main::parse_span` already accepts, so the gesture invents no second
/// format. A click in a different file, or on/above the current start,
/// restarts at a single line.
#[must_use]
pub fn anchor_click(
    current_path: &str,
    current: Option<Span>,
    path: &str,
    line: u32,
) -> (String, String) {
    // `current_path` comes from a free text input that `AnnotateSubmit` trims,
    // so compare trimmed or a stray space would silently restart the range.
    if current_path.trim() == path
        && let Some(span) = current
        && line > span.start
    {
        return (path.to_string(), format!("{}-{line}", span.start));
    }
    (path.to_string(), line.to_string())
}

/// The email a pane should authenticate its live websocket as: the reviewer's
/// typed override if they gave one, else this machine's own identity.
///
/// `None` means no identity is known anywhere, which is the only case where an
/// unauthenticated feed is still the right answer — a websocket would just fail
/// its handshake.
///
/// Exists because the annotation composer only appears once a websocket
/// connects, so "which identity do we watch as" silently decided whether the
/// pointing gesture existed at all. Defaulting it is what makes the gesture
/// reachable without the reviewer typing their own address into a form
/// (ledger `02bded62`).
#[must_use]
pub fn watch_identity(typed: &str, machine: Option<&str>) -> Option<String> {
    let typed = typed.trim();
    if !typed.is_empty() {
        return Some(typed.to_string());
    }
    machine
        .map(str::trim)
        .filter(|email| !email.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod identity_tests {
    use super::watch_identity;

    #[test]
    fn an_empty_field_falls_back_to_this_machines_identity() {
        // The reachability fix: a reviewer who types nothing still gets an
        // authenticated websocket, and therefore a composer and clickable
        // rows, on their own machine.
        assert_eq!(
            watch_identity("", Some("dan@example.com")).as_deref(),
            Some("dan@example.com")
        );
        assert_eq!(
            watch_identity("   ", Some("dan@example.com")).as_deref(),
            Some("dan@example.com")
        );
    }

    #[test]
    fn a_typed_override_always_wins_and_is_trimmed() {
        // Trimmed to match `Message::WatchEmailChanged`, so a pasted address
        // with whitespace still resolves `load_signing_key`'s lookup.
        assert_eq!(
            watch_identity(" other@example.com ", Some("dan@example.com")).as_deref(),
            Some("other@example.com")
        );
    }

    #[test]
    fn no_identity_anywhere_stays_unauthenticated() {
        // The only case where the plain SSE feed is still right: a websocket
        // would fail its handshake, and failing loudly buys nothing here.
        assert_eq!(watch_identity("", None), None);
        assert_eq!(watch_identity("", Some("")), None);
        assert_eq!(watch_identity("  ", Some("   ")), None);
    }
}

/// Up to two uppercase initials for a watcher's email, for the presence chip:
/// `dan.cieslak@x.com` → `DC`, `omp@oh-my-pi.dev` → `OM`.
///
/// Falls back to `?` when there is no usable local part, since an empty avatar
/// and an absent watcher must never look the same.
#[must_use]
pub fn watcher_initials(email: &str) -> String {
    let local = email.split('@').next().unwrap_or_default();
    let mut words = local
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty());
    let Some(first) = words.next() else {
        return "?".to_string();
    };
    let initials: String = match words.next() {
        // Two words: one initial from each ("dan.cieslak" → "dc").
        Some(second) => first
            .chars()
            .take(1)
            .chain(second.chars().take(1))
            .collect(),
        // One word: its first two characters ("omp" → "om").
        None => first.chars().take(2).collect(),
    };
    initials.to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A verbatim junto diff artifact, captured from a real session
    /// (`pointing-dogfood-20260823`, turn 1) rather than invented — including
    /// the untracked-file trailer junto appends after the last hunk, which is
    /// not unified-diff syntax.
    const REAL_ARTIFACT: &str = "\
diff --git a/lib.rs b/lib.rs
index 9275e57..3425127 100644
--- a/lib.rs
+++ b/lib.rs
@@ -1,3 +1,4 @@
 fn one() {}
 fn two() {}
 fn three() {}
+fn four() {}
diff --git a/notes.md b/notes.md
index 8728563..794d911 100644
--- a/notes.md
+++ b/notes.md
@@ -5,3 +5,4 @@ bravo
 charlie
 delta
 echo
+foxtrot

# untracked files:
?? shot.png
?? shot.ps1
";

    #[test]
    fn a_real_junto_artifact_maps_every_row_and_stops_at_the_trailer() {
        let targets = diff_row_targets(REAL_ARTIFACT);
        assert_eq!(targets.len(), REAL_ARTIFACT.lines().count());
        assert_eq!(
            targets,
            vec![
                None,                  // diff --git a/lib.rs
                None,                  // index
                None,                  // --- a/lib.rs
                None,                  // +++ b/lib.rs
                None,                  // @@ -1,3 +1,4 @@
                Some(("lib.rs", 1)),   // " fn one() {}"
                Some(("lib.rs", 2)),   // " fn two() {}"
                Some(("lib.rs", 3)),   // " fn three() {}"
                Some(("lib.rs", 4)),   // "+fn four() {}"
                None,                  // diff --git a/notes.md
                None,                  // index
                None,                  // --- a/notes.md
                None,                  // +++ b/notes.md
                None,                  // @@ -5,3 +5,4 @@ bravo
                Some(("notes.md", 5)), // " charlie"
                Some(("notes.md", 6)), // " delta"
                Some(("notes.md", 7)), // " echo"
                Some(("notes.md", 8)), // "+foxtrot"
                None,                  // blank separator — ends the hunk
                None,                  // "# untracked files:"
                None,                  // "?? shot.png"
                None,                  // "?? shot.ps1"
            ]
        );
        // Independently corroborated: the agent's own memo for this turn said
        // "`notes.md:8` — `foxtrot` added", which is the row the mapper picks.
        assert_eq!(targets[17], Some(("notes.md", 8)));
    }

    /// A second verbatim junto artifact, captured from a working tree staged to
    /// exercise every branch at once: an added file, a deleted file, a
    /// modification mixing removals and additions, pure appends, and the
    /// untracked trailer.
    const FOUR_FILES: &str = "\
diff --git a/added.rs b/added.rs
new file mode 100644
index 0000000..e14fae5
--- /dev/null
+++ b/added.rs
@@ -0,0 +1,3 @@
+fn brand_new() {
+    todo!()
+}
diff --git a/legacy.rs b/legacy.rs
deleted file mode 100644
index 69bd7f3..0000000
--- a/legacy.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-fn old_one() {}
-fn old_two() {}
diff --git a/lib.rs b/lib.rs
index 324a3b2..a4762a7 100644
--- a/lib.rs
+++ b/lib.rs
@@ -1,4 +1,5 @@
 fn one() {}
 fn two() { println!(\"two\"); }
-fn three() {}
+fn three() { println!(\"three\"); }
 fn four() {}
+fn five() {}
diff --git a/notes.md b/notes.md
index 382f027..bd0395c 100644
--- a/notes.md
+++ b/notes.md
@@ -7,3 +7,5 @@ delta
 echo
 foxtrot
 golf
+hotel
+india

# untracked files:
?? scratch.tmp
";

    #[test]
    fn a_four_file_artifact_anchors_exactly_the_new_side_of_every_hunk() {
        let targets = diff_row_targets(FOUR_FILES);
        assert_eq!(targets.len(), FOUR_FILES.lines().count());
        let anchorable: Vec<_> = targets.iter().flatten().copied().collect();
        assert_eq!(
            anchorable,
            vec![
                // An added file: its rows ARE the whole new file.
                ("added.rs", 1),
                ("added.rs", 2),
                ("added.rs", 3),
                // legacy.rs is deleted — `+++ /dev/null` leaves no path and
                // `+0,0` no line, so NOTHING in that hunk is anchorable.
                // A modification: context and `+` count, the `-` row does not,
                // so the added `fn three()` is line 3 rather than line 4.
                ("lib.rs", 1),
                ("lib.rs", 2),
                ("lib.rs", 3),
                ("lib.rs", 4),
                ("lib.rs", 5),
                // Pure appends land after the context they follow.
                ("notes.md", 7),
                ("notes.md", 8),
                ("notes.md", 9),
                ("notes.md", 10),
                ("notes.md", 11),
                // The untracked trailer contributes nothing.
            ]
        );
    }

    #[test]
    fn a_trailer_after_the_last_hunk_is_never_numbered_as_code() {
        // The bug this pins, found by running the mapper over a real artifact
        // instead of a fixture: with a lenient "unknown row is context" rule,
        // `?? shot.png` was numbered `notes.md:11` and was clickable, which
        // would have put a wrong line into a signed annotation.
        let trailing: Vec<_> = diff_row_targets(REAL_ARTIFACT)
            .into_iter()
            .skip(18)
            .collect();
        assert!(
            trailing.iter().all(Option::is_none),
            "nothing after the last hunk is anchorable, got {trailing:?}"
        );
    }

    /// The shape `workspace_diff` actually produces: `git diff` output, one
    /// file, one hunk.
    const ONE_FILE: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1234567..89abcde 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,6 +10,7 @@ fn existing() {
 context one
 context two
-removed line
+added line
+another added
 context three
";

    #[test]
    fn every_rendered_row_gets_exactly_one_target() {
        // The renderer zips this against `body.lines()`, so a length mismatch
        // would silently shift every click by one row.
        let targets = diff_row_targets(ONE_FILE);
        assert_eq!(targets.len(), ONE_FILE.lines().count());
    }

    #[test]
    fn headers_and_the_hunk_marker_are_never_anchorable() {
        let targets = diff_row_targets(ONE_FILE);
        // rows 0-3: `diff --git`, `index`, `---`, `+++`; row 4: `@@`.
        for (i, target) in targets.iter().enumerate().take(5) {
            assert_eq!(*target, None, "row {i} is a header and must not anchor");
        }
    }

    #[test]
    fn context_and_added_rows_count_new_file_lines_and_removed_rows_do_not() {
        let targets = diff_row_targets(ONE_FILE);
        // The hunk's new side starts at 10, so: context 10, context 11,
        // removed (nothing), added 12, added 13, context 14.
        assert_eq!(
            &targets[5..],
            &[
                Some(("src/lib.rs", 10)),
                Some(("src/lib.rs", 11)),
                None,
                Some(("src/lib.rs", 12)),
                Some(("src/lib.rs", 13)),
                Some(("src/lib.rs", 14)),
            ]
        );
    }

    #[test]
    fn a_second_file_switches_path_and_restarts_the_counter() {
        // The bug this pins: without resetting on `diff --git`, the second
        // file's own header rows read as context lines and stay clickable —
        // pointing at the FIRST file at bogus line numbers.
        let diff = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1,1 +1,1 @@
 first file
diff --git a/b.rs b/b.rs
index aaa..bbb 100644
--- a/b.rs
+++ b/b.rs
@@ -40,1 +50,1 @@
 second file
";
        let targets = diff_row_targets(diff);
        assert_eq!(targets[4], Some(("a.rs", 1)));
        for (i, target) in targets.iter().enumerate().skip(5).take(5) {
            assert_eq!(
                *target, None,
                "row {i} belongs to the second file's header, not the first file's body"
            );
        }
        assert_eq!(targets[10], Some(("b.rs", 50)));
    }

    #[test]
    fn multiple_hunks_each_restart_at_their_own_new_start() {
        let diff = "\
+++ b/x.rs
@@ -1,2 +1,2 @@
 one
 two
@@ -80,2 +90,2 @@
 ninety
 ninetyone
";
        let targets = diff_row_targets(diff);
        assert_eq!(
            targets,
            vec![
                None,
                None,
                Some(("x.rs", 1)),
                Some(("x.rs", 2)),
                None,
                Some(("x.rs", 90)),
                Some(("x.rs", 91)),
            ]
        );
    }

    #[test]
    fn a_deleted_file_has_no_new_side_to_point_at() {
        let diff = "\
diff --git a/gone.rs b/gone.rs
--- a/gone.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-was here
-also here
";
        assert!(
            diff_row_targets(diff).iter().all(Option::is_none),
            "a deletion leaves nothing in the new file to anchor to"
        );
    }

    #[test]
    fn a_new_file_anchors_from_line_one() {
        let diff = "\
diff --git a/new.rs b/new.rs
new file mode 100644
--- /dev/null
+++ b/new.rs
@@ -0,0 +1,2 @@
+fn main() {}
+// end
";
        let targets = diff_row_targets(diff);
        assert_eq!(
            &targets[5..],
            &[Some(("new.rs", 1)), Some(("new.rs", 2))],
            "an added file's rows are the whole new file"
        );
    }

    #[test]
    fn the_no_newline_marker_annotates_the_row_above_and_anchors_to_nothing() {
        let diff = "\
+++ b/x.rs
@@ -1,1 +1,1 @@
+no trailing newline
\\ No newline at end of file
";
        let targets = diff_row_targets(diff);
        assert_eq!(targets[2], Some(("x.rs", 1)));
        assert_eq!(targets[3], None);
    }

    #[test]
    fn an_unparseable_hunk_header_makes_its_rows_unanchorable_not_misnumbered() {
        // Refusing beats guessing: a wrong line number in a signed annotation
        // points a reviewer at the wrong code.
        let diff = "\
+++ b/x.rs
@@ garbage @@
 context
";
        assert_eq!(diff_row_targets(diff), vec![None, None, None]);
    }

    #[test]
    fn rows_before_any_file_header_are_unanchorable() {
        let diff = " orphan context\n+orphan addition\n";
        assert_eq!(diff_row_targets(diff), vec![None, None]);
    }

    #[test]
    fn a_hunk_body_line_starting_with_three_dashes_is_a_removal_not_a_header() {
        // Removing a line whose own text begins "-- " renders as "--- ".
        // Treating that as a file header would silently stop the rest of the
        // hunk from being clickable.
        let diff = "\
+++ b/x.sql
@@ -1,3 +1,2 @@
 keep
--- a comment we deleted
 after
";
        let targets = diff_row_targets(diff);
        assert_eq!(targets[2], Some(("x.sql", 1)));
        assert_eq!(targets[3], None, "it is a removed line");
        assert_eq!(
            targets[4],
            Some(("x.sql", 2)),
            "the row after it still anchors"
        );
    }

    #[test]
    fn anchor_click_on_an_empty_composer_starts_a_single_line() {
        assert_eq!(
            anchor_click("", None, "src/lib.rs", 12),
            ("src/lib.rs".to_string(), "12".to_string())
        );
    }

    #[test]
    fn anchor_click_further_down_the_same_file_extends_to_a_range() {
        let current = Span::new(12, 12).unwrap();
        assert_eq!(
            anchor_click("src/lib.rs", Some(current), "src/lib.rs", 14),
            ("src/lib.rs".to_string(), "12-14".to_string()),
            "the second click extends, in exactly the form parse_span accepts"
        );
    }

    #[test]
    fn anchor_click_extends_from_the_existing_start_not_the_existing_end() {
        let current = Span::new(12, 14).unwrap();
        assert_eq!(
            anchor_click("src/lib.rs", Some(current), "src/lib.rs", 20),
            ("src/lib.rs".to_string(), "12-20".to_string()),
            "a third click grows the same range rather than starting a new one"
        );
    }

    #[test]
    fn anchor_click_in_another_file_restarts() {
        let current = Span::new(12, 14).unwrap();
        assert_eq!(
            anchor_click("src/lib.rs", Some(current), "src/other.rs", 3),
            ("src/other.rs".to_string(), "3".to_string())
        );
    }

    #[test]
    fn anchor_click_above_or_on_the_current_start_restarts() {
        let current = Span::new(12, 14).unwrap();
        assert_eq!(
            anchor_click("src/lib.rs", Some(current), "src/lib.rs", 5),
            ("src/lib.rs".to_string(), "5".to_string()),
            "clicking above the start reads as picking a new start"
        );
        assert_eq!(
            anchor_click("src/lib.rs", Some(current), "src/lib.rs", 12),
            ("src/lib.rs".to_string(), "12".to_string()),
            "re-clicking the start collapses the range instead of no-oping"
        );
    }

    #[test]
    fn anchor_click_tolerates_a_hand_typed_path_with_whitespace() {
        // `annotate_path` is a free text input; `AnnotateSubmit` trims it, so
        // the extend check must compare trimmed too or a stray space would
        // silently restart the range.
        let current = Span::new(12, 12).unwrap();
        assert_eq!(
            anchor_click(" src/lib.rs ", Some(current), "src/lib.rs", 14),
            ("src/lib.rs".to_string(), "12-14".to_string())
        );
    }

    #[test]
    fn anchor_click_with_unparseable_current_lines_starts_fresh() {
        assert_eq!(
            anchor_click("src/lib.rs", None, "src/lib.rs", 14),
            ("src/lib.rs".to_string(), "14".to_string())
        );
    }

    #[test]
    fn watcher_initials_reads_a_dotted_local_part_as_two_initials() {
        assert_eq!(watcher_initials("dan.cieslak@example.com"), "DC");
        assert_eq!(watcher_initials("a-b@example.com"), "AB");
        assert_eq!(watcher_initials("a_b@example.com"), "AB");
    }

    #[test]
    fn watcher_initials_reads_a_single_word_local_part_as_its_first_two_chars() {
        assert_eq!(watcher_initials("omp@oh-my-pi.dev"), "OM");
        assert_eq!(
            watcher_initials("dcieslak19973@users.noreply.github.com"),
            "DC"
        );
    }

    #[test]
    fn watcher_initials_never_renders_blank() {
        // A blank avatar and an absent watcher must not look the same.
        assert_eq!(watcher_initials(""), "?");
        assert_eq!(watcher_initials("@example.com"), "?");
        assert_eq!(watcher_initials("..@example.com"), "?");
    }

    #[test]
    fn watcher_initials_handles_a_one_character_local_part() {
        assert_eq!(watcher_initials("x@example.com"), "X");
    }
}
