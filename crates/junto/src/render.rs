//! Rendering a channel projection for readers.
//!
//! Two audiences, one source of truth (the [`ChannelView`] projection):
//! - [`brief_markdown`] — the **agent** read path: the MCP `view_channel`
//!   tool and the `/channels/{name}/brief` endpoint the SessionStart recall
//!   hook injects into agent context (`docs/adr/0013`). **Scaled**: state,
//!   not history — recall must not grow linearly with the record. The full
//!   transcript ([`transcript_markdown`]) stays one call away
//!   (`view_channel` with `full: true`).
//! - [`index_html`] / [`channel_html`] — the **human** read path: the pages
//!   the desktop shell frames (`docs/adr/0018`). Server-rendered with shared
//!   app chrome (sidebar navigation, dark theme) and almost no JS —
//!   `<details>` carries the expand/collapse; the single inline script is
//!   act feedback ([`ACT_FEEDBACK_SCRIPT`]), pure progressive enhancement.
//!   Its information design is product surface (`docs/adr/0013`), reviewed
//!   as such.

use junto_kernel::{
    ChannelId, ChannelView, EntryId, EntryPayload, GateStatus, LedgerEntry, Member, MemberKind,
    ProvenanceRef, SessionState, Standing,
};
use std::fmt::Write as _;

use crate::host::{AttentionGroup, AttentionKind, ChannelSummary};

/// `Name <email>` plus an `(agent)` marker — humans are the unmarked case.
fn member_label(member: &Member) -> String {
    let marker = match member.kind {
        MemberKind::Human => "",
        MemberKind::Agent => " (agent)",
    };
    format!("{} <{}>{marker}", member.display_name, member.email)
}

/// Tier knobs for the scaled brief (🔵): how much settled history a newcomer
/// is handed before the rest decays to a count and a pointer.
const BRIEF_RECENT_FULL: usize = 10;
const BRIEF_OLDER_CLAMPED: usize = 15;
const BRIEF_SANCTIONED_RECENT: usize = 5;
const BRIEF_RECENT_TAIL: usize = 5;
/// Clamp width (chars) for tiered-down statements; a sentence boundary wins.
const BRIEF_CLAMP: usize = 200;

/// Trim `text` to its first sentence, or hard-cut at `max` chars with an
/// ellipsis — the tiered-down rendering of settled statements (the claim
/// survives; the body stays one `view_channel` away).
fn clamp(text: &str, max: usize) -> String {
    let text = text.trim();
    if let Some(pos) = text.find(". ")
        && pos < max
    {
        return text[..=pos].trim_end().to_string();
    }
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

/// The first 8 chars of an entry id — enough for the MCP tools' git-style
/// prefix resolution (≥6 hex chars), a fraction of the bytes.
fn short(id: &junto_kernel::EntryId) -> String {
    let full = id.to_string();
    full.chars().take(8).collect()
}

/// One verification act recorded against a target, for folding into the
/// target's own brief line instead of renting a line of its own. (The act's
/// rationale stays in the full transcript — the brief carries the verdict
/// and the verifier.)
struct ActNote<'a> {
    verb: &'static str,
    author: &'a Member,
}

/// Every verification act in the view, grouped by target.
fn act_notes(view: &ChannelView) -> std::collections::HashMap<EntryId, Vec<ActNote<'_>>> {
    let mut notes: std::collections::HashMap<EntryId, Vec<ActNote>> = Default::default();
    for entry in &view.entries {
        let (verb, target) = match &entry.payload {
            EntryPayload::Ratification { target, .. } => ("ratified", target),
            EntryPayload::Park { target, .. } => ("parked", target),
            EntryPayload::Approval { target, .. } => ("approved", target),
            EntryPayload::Rejection { target, .. } => ("rejected", target),
            _ => continue,
        };
        notes.entry(*target).or_default().push(ActNote {
            verb,
            author: &entry.author,
        });
    }
    notes
}

/// The last act of a given verb against a target, e.g. who ratified it.
fn last_act<'a>(
    notes: &'a std::collections::HashMap<EntryId, Vec<ActNote<'a>>>,
    target: &EntryId,
    verb: &str,
) -> Option<&'a ActNote<'a>> {
    notes
        .get(target)?
        .iter()
        .rev()
        .find(|note| note.verb == verb)
}

/// The scaled brief's partition of a channel's live entries — what a
/// returning member needs each entry *for*. Shared by the agent brief
/// ([`brief_markdown`]) and the human channel page ([`channel_html`]): one
/// information design, two renderings (`docs/adr/0013`).
struct BriefShape<'a> {
    /// Provisional assertions and pending proposals — the act targets.
    open: Vec<&'a LedgerEntry>,
    /// Ratified assertions plus corrections (the live text of settled
    /// territory), in canonical order.
    ratified: Vec<&'a LedgerEntry>,
    /// Parked dead-ends — resolved, surfaced on demand, never decayed away.
    parked: Vec<&'a LedgerEntry>,
    /// Approved proposals — sanctioned actions, the fastest-decaying class.
    approved: Vec<&'a LedgerEntry>,
    /// Rejected proposals — resolved like parked.
    rejected: Vec<&'a LedgerEntry>,
    /// Superseded assertions, collapsed into their corrections.
    superseded: usize,
}

/// Partition the live entries by what a newcomer needs them for. Genesis,
/// membership, act, and session entries don't rent lines here: the party
/// line, the folded notes, and the sessions board carry them.
fn brief_shape(view: &ChannelView) -> BriefShape<'_> {
    let mut shape = BriefShape {
        open: Vec::new(),
        ratified: Vec::new(),
        parked: Vec::new(),
        approved: Vec::new(),
        rejected: Vec::new(),
        superseded: 0,
    };
    for entry in &view.entries {
        match &entry.payload {
            EntryPayload::Assertion { .. } => match view.standing(&entry.id) {
                Some(Standing::Provisional) => shape.open.push(entry),
                Some(Standing::Ratified) => shape.ratified.push(entry),
                Some(Standing::Parked) => shape.parked.push(entry),
                // Collapsed: the correction carries the live text.
                Some(Standing::Superseded) => shape.superseded += 1,
                None => {}
            },
            // A correction of an assertion is the live text of settled
            // territory. A correction of anything else (the genesis — a
            // rename, docs/adr/0016) is not a decision and rents no line.
            EntryPayload::Correction { target, .. } if view.standings.contains_key(target) => {
                shape.ratified.push(entry);
            }
            EntryPayload::Correction { .. } => {}
            EntryPayload::Proposal { .. } => match view.gate_status(&entry.id) {
                Some(GateStatus::Pending) => shape.open.push(entry),
                Some(GateStatus::Approved) => shape.approved.push(entry),
                Some(GateStatus::Rejected) => shape.rejected.push(entry),
                None => {}
            },
            _ => {}
        }
    }
    shape
}

/// The agent-facing **scaled brief**: state, not history. Acts fold into
/// their targets, superseded bodies collapse, and old resolved material
/// decays by tier (recent ratified in full → older clamped → oldest a count)
/// — recall must not grow linearly with the record (`docs/adr/0013`; the
/// full transcript stays one call away via [`transcript_markdown`]).
///
/// Retention classes: open items and parked/rejected entries never decay
/// (the former are actionable, the latter exist to prevent re-treads);
/// ratified decisions tier down; sanctioned (approved) actions decay
/// fastest — a delivered action is a completion record, embodied in the
/// work itself.
pub fn brief_markdown(name: &str, id: &ChannelId, view: &ChannelView) -> String {
    let mut out = format!("# channel '{name}' ({id}) — brief\n\n");
    if !view.party.is_empty() {
        let roster: Vec<String> = view.party.iter().map(member_label).collect();
        let _ = writeln!(
            out,
            "party (founder first; only members' entries count — docs/adr/0017): {}\n",
            roster.join(", ")
        );
    }
    if view.entries.is_empty() {
        out.push_str("(no entries)\n");
        return out;
    }
    if view.closed {
        out.push_str(
            "**This channel is closed** (`docs/adr/0022`) — the inquiry left the working \
             set; do not record new work here without reopening.\n\n",
        );
    }
    out.push_str(
        "State, not history: verification acts are folded into their targets and old \
         resolved material decays out. The full transcript is one call away \
         (`view_channel` with `full: true`).\n\n",
    );

    let notes = act_notes(view);
    let BriefShape {
        open,
        ratified,
        parked,
        approved,
        rejected,
        superseded,
    } = brief_shape(view);

    // Needs attention — full ids and full detail: these are the act targets.
    out.push_str("## needs attention — act by id (ratify/park · approve/reject)\n\n");
    if open.is_empty() {
        out.push_str("(nothing pending)\n");
    } else {
        for entry in &open {
            let _ = writeln!(
                out,
                "- `{}` @{} {}: {}",
                entry.id,
                entry.timestamp.as_millis(),
                member_label(&entry.author),
                describe_markdown(entry, view)
            );
            if let EntryPayload::Assertion { rationale, .. }
            | EntryPayload::Proposal { rationale, .. } = &entry.payload
            {
                let _ = writeln!(out, "  why: {rationale}");
            }
        }
    }

    // Standing decisions, newest first: recent in full, older clamped to the
    // claim, the rest a count.
    out.push_str(
        "\n## standing decisions (newest first — do not contradict without surfacing)\n\n",
    );
    if ratified.is_empty() {
        out.push_str("(none yet)\n");
    }
    for (index, entry) in ratified.iter().rev().enumerate() {
        if index >= BRIEF_RECENT_FULL + BRIEF_OLDER_CLAMPED {
            let _ = writeln!(
                out,
                "- …and {} older standing decisions (consult the full transcript before \
                 overturning settled territory)",
                ratified.len() - index
            );
            break;
        }
        let (text, correction_of) = match &entry.payload {
            EntryPayload::Assertion { statement, .. } => (statement.as_str(), None),
            EntryPayload::Correction {
                target, statement, ..
            } => (statement.as_str(), Some(*target)),
            _ => continue,
        };
        let body = if index < BRIEF_RECENT_FULL {
            text.to_string()
        } else {
            clamp(text, BRIEF_CLAMP)
        };
        let corrects = correction_of
            .map(|target| format!(" (correction of `{}`)", short(&target)))
            .unwrap_or_default();
        let by = last_act(&notes, &entry.id, "ratified")
            .map(|note| format!(" — ratified by {}", note.author.display_name))
            .unwrap_or_default();
        let _ = writeln!(out, "- `{}`{corrects} {body}{by}", short(&entry.id));
    }

    // Parked dead-ends and rejected proposals are *resolved*: they do not
    // rent standing lines (Dan, 2026-06-11 — a dead-end belongs in recall at
    // the moment a path starts coming back from the dead, which is a
    // relevance trigger, not a standing section). Fresh ones still surface
    // in the "recently" tail; all of them live in the full transcript, and
    // the footer counts keep their existence visible.

    // Sanctioned actions decay fastest: once delivered they are completion
    // records, embodied in the work itself.
    if !approved.is_empty() {
        out.push_str("\n## sanctioned actions (approved gates; newest first)\n\n");
        for (index, entry) in approved.iter().rev().enumerate() {
            if index >= BRIEF_SANCTIONED_RECENT {
                let _ = writeln!(
                    out,
                    "- …and {} older sanctioned actions",
                    approved.len() - index
                );
                break;
            }
            let EntryPayload::Proposal { action, .. } = &entry.payload else {
                continue;
            };
            let by = last_act(&notes, &entry.id, "approved")
                .map(|note| format!(" — approved by {}", note.author.display_name))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "- `{}` {}{by}",
                short(&entry.id),
                clamp(action, BRIEF_CLAMP)
            );
        }
    }

    // A short chronological tail for resumption: what just happened here.
    out.push_str("\n## recently\n\n");
    let tail_start = view.entries.len().saturating_sub(BRIEF_RECENT_TAIL);
    for entry in &view.entries[tail_start..] {
        let _ = writeln!(
            out,
            "- {} {}",
            entry.author.display_name,
            recent_line(entry)
        );
    }

    let folded: usize = notes.values().map(Vec::len).sum();
    let _ = writeln!(
        out,
        "\n({folded} verification acts folded into the lines above; {superseded} superseded \
         entries collapsed into their corrections; {} parked dead-ends and {} rejected \
         proposals omitted — call the `dead_ends` tool before re-trying an approach that \
         may have been tried)",
        parked.len(),
        rejected.len(),
    );
    out
}

/// One entry as a clamped one-liner for the brief's "recently" tail.
fn recent_line(entry: &LedgerEntry) -> String {
    const TAIL_CLAMP: usize = 80;
    match &entry.payload {
        EntryPayload::ChannelOpened { name } => format!("opened channel '{name}'"),
        EntryPayload::MemberAdded { member } => {
            format!("added member {}", member.display_name)
        }
        EntryPayload::Assertion { statement, .. } => {
            format!("asserted: {}", clamp(statement, TAIL_CLAMP))
        }
        EntryPayload::Proposal { action, .. } => {
            format!("proposed: {}", clamp(action, TAIL_CLAMP))
        }
        EntryPayload::Correction { target, .. } => format!("corrected `{}`", short(target)),
        EntryPayload::Ratification { target, .. } => format!("ratified `{}`", short(target)),
        EntryPayload::Park { target, .. } => format!("parked `{}`", short(target)),
        EntryPayload::Approval { target, .. } => format!("approved `{}`", short(target)),
        EntryPayload::Rejection { target, .. } => format!("rejected `{}`", short(target)),
        EntryPayload::SessionStarted { intent, .. } => {
            format!("started a session: {}", clamp(intent, TAIL_CLAMP))
        }
        EntryPayload::SessionUpdated { target, .. } => {
            format!("updated session `{}`", short(target))
        }
        EntryPayload::ArtifactAttached { description, .. } => {
            format!("attached artifact: {}", clamp(description, TAIL_CLAMP))
        }
        EntryPayload::ChannelClosed { rationale } => {
            format!("closed the channel: {}", clamp(rationale, TAIL_CLAMP))
        }
        EntryPayload::ChannelReopened { rationale } => {
            format!("reopened the channel: {}", clamp(rationale, TAIL_CLAMP))
        }
    }
}

/// The most dead-ends one `dead_ends` call returns — the output is bounded
/// no matter how much history accumulates (🔵).
const DEAD_ENDS_LIMIT: usize = 5;

/// Lowercased word tokens for the dead-end similarity ranking: alphanumeric
/// runs of 3+ chars (shorter ones are mostly stopword noise).
fn tokens(text: &str) -> std::collections::HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.len() >= 3)
        .map(str::to_string)
        .collect()
}

/// One dead-end (a parked assertion or rejected proposal) plus its killing
/// act, gathered for ranking.
struct DeadEnd<'a> {
    entry: &'a LedgerEntry,
    label: &'static str,
    text: &'a str,
    kill: Option<(&'a LedgerEntry, &'a str)>,
}

/// The on-demand dead-end surface — the counterpart of the scaled brief's
/// deliberate omission (Dan, 2026-06-11: a dead-end belongs in recall when
/// its path starts coming back from the dead). With `about`, a **similarity
/// search**: dead-ends ranked by token overlap with the query, rare words
/// weighing more (IDF) — lexical, local, deterministic; an embedding-based
/// `MemoryProvider` is the designed upgrade path (`docs/pluggability.md`).
/// Without `about`, the most recent dead-ends. Either way the output is
/// bounded at [`DEAD_ENDS_LIMIT`] — recall surfaces must not grow with the
/// record.
pub fn dead_ends_markdown(name: &str, view: &ChannelView, about: Option<&str>) -> String {
    // The killing act's rationale is the value here: collect them verbatim.
    let mut kills: std::collections::HashMap<EntryId, (&LedgerEntry, &str)> = Default::default();
    for entry in &view.entries {
        match &entry.payload {
            EntryPayload::Park { target, rationale }
            | EntryPayload::Rejection { target, rationale } => {
                kills.insert(*target, (entry, rationale));
            }
            _ => {}
        }
    }

    let dead_ends: Vec<DeadEnd> = view
        .entries
        .iter()
        .filter_map(|entry| {
            let (label, text) = match &entry.payload {
                EntryPayload::Assertion { statement, .. }
                    if view.standing(&entry.id) == Some(Standing::Parked) =>
                {
                    ("parked", statement.as_str())
                }
                EntryPayload::Proposal { action, .. }
                    if view.gate_status(&entry.id) == Some(GateStatus::Rejected) =>
                {
                    ("rejected", action.as_str())
                }
                _ => return None,
            };
            Some(DeadEnd {
                entry,
                label,
                text,
                kill: kills.get(&entry.id).copied(),
            })
        })
        .collect();

    let mut out = format!("# dead-ends in '{name}'\n\n");
    let total = dead_ends.len();
    if total == 0 {
        out.push_str("(no dead-ends — nothing parked or rejected in this channel)\n");
        return out;
    }

    // Each dead-end's searchable text: the claim plus why it was killed.
    let docs: Vec<std::collections::HashSet<String>> = dead_ends
        .iter()
        .map(|dead_end| {
            tokens(&format!(
                "{} {}",
                dead_end.text,
                dead_end.kill.map(|(_, why)| why).unwrap_or_default()
            ))
        })
        .collect();

    let ranked: Vec<&DeadEnd> = match about.map(tokens) {
        Some(query) if !query.is_empty() => {
            // IDF-weighted token overlap, normalized against doc length so
            // long dead-ends don't win by surface area. Crude next to
            // embeddings, but local, deterministic, and dependency-free.
            let idf = |token: &str| {
                let with = docs.iter().filter(|doc| doc.contains(token)).count();
                ((1.0 + total as f64) / (1.0 + with as f64)).ln() + 1.0
            };
            let mut scored: Vec<(f64, &DeadEnd)> = dead_ends
                .iter()
                .zip(&docs)
                .filter_map(|(dead_end, doc)| {
                    let score: f64 = query
                        .iter()
                        .filter(|token| doc.contains(*token))
                        .map(|token| idf(token))
                        .sum();
                    (score > 0.0).then(|| (score / (doc.len() as f64).sqrt(), dead_end))
                })
                .collect();
            scored.sort_by(|a, b| b.0.total_cmp(&a.0));
            scored
                .into_iter()
                .take(DEAD_ENDS_LIMIT)
                .map(|(_, dead_end)| dead_end)
                .collect()
        }
        // No query: the most recent dead-ends (entries are in canonical,
        // oldest-first order).
        _ => dead_ends.iter().rev().take(DEAD_ENDS_LIMIT).collect(),
    };

    if ranked.is_empty() {
        let _ = writeln!(
            out,
            "(none of the {total} dead-ends resemble '{}' — the match is lexical, so try \
             other words for the same idea before concluding the territory is untried)",
            about.unwrap_or_default()
        );
        return out;
    }

    for dead_end in &ranked {
        let _ = writeln!(
            out,
            "- `{}` [{}] {} <{}>: {}",
            dead_end.entry.id,
            dead_end.label,
            dead_end.entry.author.display_name,
            dead_end.entry.author.email,
            dead_end.text
        );
        if let Some((act, rationale)) = dead_end.kill {
            let _ = writeln!(
                out,
                "  {} by {} @{}: {rationale}",
                dead_end.label,
                act.author.display_name,
                act.timestamp.as_millis()
            );
        }
    }
    let how = if about.is_some() {
        "most similar first"
    } else {
        "most recent first"
    };
    let _ = writeln!(
        out,
        "\n({} of {total} dead-ends, {how} — do not re-try these without surfacing the \
         prior park/rejection to the party)",
        ranked.len()
    );
    out
}

/// The full transcript: every entry in canonical order with ids and derived
/// states — the detail path behind the scaled [`brief_markdown`]
/// (`view_channel` with `full: true`).
pub fn transcript_markdown(name: &str, id: &ChannelId, view: &ChannelView) -> String {
    let mut out = format!("# channel '{name}' ({id})\n\n");
    if !view.party.is_empty() {
        let roster: Vec<String> = view.party.iter().map(member_label).collect();
        let _ = writeln!(
            out,
            "party (founder first; only members' entries count — docs/adr/0017): {}\n",
            roster.join(", ")
        );
    }
    if view.entries.is_empty() {
        out.push_str("(no entries)\n");
        return out;
    }
    let _ = writeln!(out, "{} entries, canonical order:\n", view.entries.len());
    for entry in &view.entries {
        let when = entry.timestamp.as_millis();
        let who = format!("{} <{}>", entry.author.display_name, entry.author.email);
        let marker = if view.unrecognized.contains(&entry.id) {
            " [unrecognized author — not in the party; excluded from projection]"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "- `{}` @{when} {who}:{marker} {}",
            entry.id,
            describe_markdown(entry, view)
        );
    }
    out
}

/// One entry on one markdown line, with its derived state attached.
fn describe_markdown(entry: &LedgerEntry, view: &ChannelView) -> String {
    match &entry.payload {
        EntryPayload::ChannelOpened { name } => {
            format!("**genesis** — channel '{name}' opened")
        }
        EntryPayload::MemberAdded { member } => {
            format!("**member added** — {}", member_label(member))
        }
        EntryPayload::Assertion {
            statement, frame, ..
        } => {
            format!(
                "**assertion** [{}] — {statement}{}",
                standing_label(view, entry),
                frame_markdown(frame.as_ref())
            )
        }
        EntryPayload::Ratification { target, .. } => format!("ratification of `{target}`"),
        EntryPayload::Park { target, .. } => format!("park of `{target}`"),
        EntryPayload::Correction {
            target, statement, ..
        } => format!("correction of `{target}` — {statement}"),
        EntryPayload::Proposal { action, frame, .. } => {
            format!(
                "**proposal** [{}] — {action}{}",
                gate_label(view, entry),
                frame_markdown(frame.as_ref())
            )
        }
        EntryPayload::Approval { target, .. } => format!("approval of `{target}`"),
        EntryPayload::Rejection { target, .. } => format!("rejection of `{target}`"),
        EntryPayload::SessionStarted { intent } => {
            format!(
                "**agent session** [{}] — {intent}",
                session_label(view, entry)
            )
        }
        EntryPayload::SessionUpdated {
            target,
            state,
            note,
        } => {
            format!(
                "session update of `{target}` → {} — {note}",
                session_state_label(*state)
            )
        }
        EntryPayload::ArtifactAttached {
            target,
            kind,
            description,
            ..
        } => {
            format!("**artifact** ({kind}) on session `{target}` — {description}")
        }
        EntryPayload::ChannelClosed { rationale } => {
            format!("**channel closed** — {rationale}")
        }
        EntryPayload::ChannelReopened { rationale } => {
            format!("**channel reopened** — {rationale}")
        }
    }
}

/// A decision frame on one brief line, so agent verifiers see the options
/// too (`docs/adr/0019`).
fn frame_markdown(frame: Option<&junto_kernel::DecisionFrame>) -> String {
    let Some(frame) = frame else {
        return String::new();
    };
    let options: Vec<String> = frame
        .options
        .iter()
        .map(|option| format!("\"{}\"→{}", option.label, frame_act_route(option.act)))
        .collect();
    format!(" [options: {}]", options.join(" · "))
}

/// The act route segment a [`junto_kernel::FrameAct`] maps to.
fn frame_act_route(act: junto_kernel::FrameAct) -> &'static str {
    match act {
        junto_kernel::FrameAct::Ratify => "ratify",
        junto_kernel::FrameAct::Park => "park",
        junto_kernel::FrameAct::Approve => "approve",
        junto_kernel::FrameAct::Reject => "reject",
    }
}

/// An assertion's derived standing as a lowercase label.
fn standing_label(view: &ChannelView, entry: &LedgerEntry) -> &'static str {
    match view.standing(&entry.id) {
        Some(Standing::Provisional) => "provisional",
        Some(Standing::Ratified) => "ratified",
        Some(Standing::Parked) => "parked",
        Some(Standing::Superseded) => "superseded",
        None => "unknown",
    }
}

/// A proposal's derived gate status as a lowercase label.
fn gate_label(view: &ChannelView, entry: &LedgerEntry) -> &'static str {
    match view.gate_status(&entry.id) {
        Some(GateStatus::Pending) => "pending",
        Some(GateStatus::Approved) => "approved",
        Some(GateStatus::Rejected) => "rejected",
        None => "unknown",
    }
}

/// A [`SessionState`] as a lowercase label (badge class + display text).
fn session_state_label(state: SessionState) -> &'static str {
    match state {
        SessionState::Working => "working",
        SessionState::Blocked => "blocked",
        SessionState::AwaitingApproval => "awaiting-approval",
        SessionState::Done => "done",
        SessionState::Error => "error",
    }
}

/// An Agent Session's derived state as a lowercase label.
fn session_label(view: &ChannelView, entry: &LedgerEntry) -> &'static str {
    view.session(&entry.id)
        .map_or("unknown", |session| session_state_label(session.state))
}

// ---- the human pages (the pixels the desktop shell frames) ----

/// The shared app chrome: wordmark + channel sidebar on the left, `content`
/// on the right. Every page is this shell with a different main pane.
fn page_shell(
    title: &str,
    nav: &[ChannelSummary],
    active: Option<&ChannelId>,
    content: &str,
) -> String {
    // Channels group under their home substrate (labelled by directory
    // name), groups ordered by first appearance in `nav` — which arrives
    // most-recently-active first, so the busiest repo tops the sidebar. A
    // storage fact used as a reading aid, not scope (docs/domain-model.md).
    // Closed channels leave the sidebar entirely (docs/adr/0022) — they
    // remain reachable from the index's archive section.
    let open_nav: Vec<&ChannelSummary> = nav.iter().filter(|s| !s.closed).collect();
    let mut group_order: Vec<&std::path::PathBuf> = Vec::new();
    for summary in &open_nav {
        if !group_order.contains(&&summary.substrate) {
            group_order.push(&summary.substrate);
        }
    }
    let mut links = String::new();
    for substrate in group_order {
        if open_nav.iter().any(|s| &s.substrate != substrate) {
            // Only label groups when there is more than one substrate —
            // a single-repo host needs no heading.
            let label = substrate
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| substrate.display().to_string());
            let _ = writeln!(
                links,
                "<div class=\"side-sub\" title=\"{path}\">{label}</div>",
                path = escape_html(&substrate.display().to_string()),
                label = escape_html(&label),
            );
        }
        for summary in open_nav.iter().filter(|s| &s.substrate == substrate) {
            let display_name = summary.name.as_deref().unwrap_or("(unopened)");
            let href = summary
                .name
                .clone()
                .unwrap_or_else(|| summary.id.to_string());
            let class = if active == Some(&summary.id) {
                "chan active"
            } else {
                "chan"
            };
            let gates = if summary.open_gates > 0 {
                format!("<span class=\"gatecount\">{}</span>", summary.open_gates)
            } else {
                String::new()
            };
            let _ = writeln!(
                links,
                "<a class=\"{class}\" href=\"/channels/{href}\"><span class=\"chan-name\">{name}</span>{gates}</a>",
                href = escape_html(&href),
                name = escape_html(display_name),
            );
        }
    }
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{title}</title>\n<style>{CSS}</style></head>\n\
         <body><div class=\"app\">\n\
         <nav class=\"side\">\n\
         <a class=\"brand\" href=\"/\"><span class=\"logo\">j</span>junto</a>\n\
         <div class=\"side-label\">channels</div>\n{links}\
         <details class=\"side-menu\"><summary>+ new</summary>\
         <a class=\"chan open-link\" href=\"/new#open-channel\">open a channel…</a>\
         <a class=\"chan open-link\" href=\"/new#setup-repo\">set up a repo…</a>\
         </details>\n\
         </nav>\n\
         <main>\n{content}</main>\n\
         </div>{ACT_FEEDBACK_SCRIPT}</body></html>\n",
        title = escape_html(title),
    )
}

/// The channel index — every channel across every registered home substrate,
/// the landing page of the one surface (`docs/adr/0015`). Leads with the
/// focus board (what needs you, grouped by inquiry — `docs/attention.md`),
/// then the channel cards: who is on each, how alive it is, the latest entry.
pub fn index_html(summaries: &[ChannelSummary], attention: &[AttentionGroup]) -> String {
    let mut cards = String::new();
    let mut closed_cards = String::new();
    for summary in summaries {
        let display_name = summary.name.as_deref().unwrap_or("(unopened)");
        let href = summary
            .name
            .clone()
            .unwrap_or_else(|| summary.id.to_string());
        let gates = if summary.closed {
            "<span class=\"badge quiet\">closed</span>".to_string()
        } else if summary.open_gates > 0 {
            format!(
                "<span class=\"badge pending\">{} open gate{}</span>",
                summary.open_gates,
                if summary.open_gates == 1 { "" } else { "s" }
            )
        } else {
            "<span class=\"badge quiet\">no open gates</span>".to_string()
        };
        let preview = summary
            .latest
            .as_deref()
            .map(|latest| format!("<div class=\"preview\">{}</div>", escape_html(latest)))
            .unwrap_or_default();
        let when = summary
            .last_activity
            .map(|ts| format!(" · active {}", escape_html(&ago(ts.as_millis()))))
            .unwrap_or_default();
        let members = if summary.members > 0 {
            format!(
                " · {} member{}",
                summary.members,
                if summary.members == 1 { "" } else { "s" }
            )
        } else {
            String::new()
        };
        let card = format!(
            "<a class=\"card-link\" href=\"/channels/{href}\"><article class=\"card chan-card\">\
             <header><h2>{name}</h2><span class=\"spacer\"></span>{gates}</header>\
             {preview}\
             <div class=\"meta-line\">{count} entries{members}{when}</div>\
             <footer class=\"id\">{id} · {substrate}</footer>\
             </article></a>\n",
            href = escape_html(&href),
            name = escape_html(display_name),
            count = summary.entry_count,
            id = summary.id,
            substrate = escape_html(&summary.substrate.display().to_string()),
        );
        // Closed channels archive below (docs/adr/0022): present, demoted.
        if summary.closed {
            closed_cards.push_str(&card);
        } else {
            cards.push_str(&card);
        }
    }
    let open_count = summaries.iter().filter(|s| !s.closed).count();
    let body = if open_count == 0 {
        "<p class=\"empty\">no open channels — open one below</p>".to_string()
    } else {
        format!("<div class=\"cards\">{cards}</div>")
    };
    let archive = if closed_cards.is_empty() {
        String::new()
    } else {
        format!(
            "<details class=\"ledger\"><summary class=\"board-head\">closed channels \
             ({})</summary>\n<div class=\"cards\">{closed_cards}</div></details>\n",
            summaries.len() - open_count
        )
    };
    let open_gates: usize = summaries.iter().map(|summary| summary.open_gates).sum();
    let gates_note = if open_gates > 0 {
        format!(
            " · <span class=\"attention\">{open_gates} gate{} awaiting verification</span>",
            if open_gates == 1 { "" } else { "s" }
        )
    } else {
        String::new()
    };
    let content = format!(
        "<h1>channels</h1>\n\
         <p class=\"meta\">{count} channel{plural} across every registered substrate\
         {gates_note}</p>\n{board}\n<h2 class=\"board-head\">all channels</h2>\n{body}\n\
         {archive}",
        count = summaries.len(),
        plural = if summaries.len() == 1 { "" } else { "s" },
        board = focus_board(attention, "/"),
    );
    page_shell("junto — channels", summaries, None, &content)
}

/// The "/new" page — where the sidebar's "+ new" menu lands: open a channel
/// and set up a repo, each form with room to breathe instead of renting the
/// bottom of the index.
pub fn new_html(nav: &[ChannelSummary], substrates: &[std::path::PathBuf]) -> String {
    let content = format!(
        "<h1>new</h1>\n\
         <p class=\"meta\">open a unit of inquiry, or bring another repo onto the one \
         surface</p>\n{open_form}{repo_form}",
        open_form = open_channel_form(substrates),
        repo_form = setup_repo_form(),
    );
    page_shell("junto — new", nav, None, &content)
}

/// The set-up-a-repo form: the terminal-less `junto init`. Registers the
/// repo as a home substrate, wires the agent harness, and opens its ambient
/// channel (named after the directory unless overridden).
fn setup_repo_form() -> String {
    "<section class=\"board\" id=\"setup-repo\"><h2 class=\"board-head\">set up a repo</h2>\n\
     <form class=\"act open-channel\" method=\"post\" action=\"/repos\">\
     <input name=\"path\" placeholder=\"path to a git repo, e.g. D:\\git\\my-project\" required>\
     <input name=\"channel\" placeholder=\"ambient channel name (default: the directory name)\">\
     <button class=\"primary\">set up</button>\
     </form>\
     <p class=\"meta\">registers the repo as a home substrate, wires the agent harness \
     (.mcp.json + recall hook), and opens its ambient channel — everything `junto init` \
     does except granting an agent membership</p></section>"
        .to_string()
}

/// The open-a-channel form: name plus, when the host serves several
/// substrates, a home-substrate picker. The founder is the substrate's git
/// user — no identity input, no member code (`docs/adr/0021`).
fn open_channel_form(substrates: &[std::path::PathBuf]) -> String {
    let picker = if substrates.len() > 1 {
        let options: Vec<String> = substrates
            .iter()
            .map(|repo| {
                let path = escape_html(&repo.display().to_string());
                format!("<option value=\"{path}\">{path}</option>")
            })
            .collect();
        format!(
            "<select name=\"repo\" title=\"the home substrate — where this channel's \
             durable record lives (docs/adr/0014)\">{}</select>",
            options.join("")
        )
    } else {
        // One (or zero) substrates: the host picks it; no field to fill.
        String::new()
    };
    format!(
        "<section class=\"board\" id=\"open-channel\">\
         <h2 class=\"board-head\">open a channel</h2>\n\
         <form class=\"act open-channel\" method=\"post\" action=\"/channels\">\
         <input name=\"name\" placeholder=\"a name for one unit of inquiry, e.g. \
         payments-refactor\" required>\
         {picker}\
         <button class=\"primary\">open</button>\
         </form></section>"
    )
}

/// Milliseconds-epoch → a human resumption cue ("12m ago"), falling back to
/// the date once it stops being recent.
fn ago(millis: i64) -> String {
    match wait_time(millis) {
        Some(duration) => format!("{duration} ago"),
        None => iso_utc(millis),
    }
}

/// Milliseconds-epoch → elapsed duration ("12m", "3h", "5d"); `None` once it
/// is old enough that a date reads better.
fn wait_time(millis: i64) -> Option<String> {
    let now = junto_kernel::Timestamp::now().as_millis();
    let minutes = now.saturating_sub(millis) / 60_000;
    match minutes {
        ..=0 => Some("moments".to_string()),
        1..=59 => Some(format!("{minutes}m")),
        60..=1439 => Some(format!("{}h", minutes / 60)),
        1440..=43_199 => Some(format!("{}d", minutes / 1440)),
        _ => None,
    }
}

// ---- the focus board (docs/attention.md) ----

/// The needs-you board: every act awaiting the member, grouped by inquiry —
/// never a flat list. Renders the load line, the groups (gate-bearing
/// inquiries first, as ordered by the host), and an all-clear empty state.
/// `back` is where the inline act forms return after acting.
pub fn focus_board(groups: &[AttentionGroup], back: &str) -> String {
    if groups.is_empty() {
        return "<section class=\"board\"><h2 class=\"board-head\">needs you</h2>\
                <p class=\"all-clear\">all clear — nothing awaits your verification</p>\
                </section>\n"
            .to_string();
    }
    let item_count: usize = groups.iter().map(|group| group.items.len()).sum();
    let mut sections = String::new();
    for group in groups {
        let _ = writeln!(sections, "{}", attention_group(group, back, true));
    }
    format!(
        "<section class=\"board\"><h2 class=\"board-head\">needs you</h2>\
         <p class=\"meta\">{inquiries} inquir{ies} in flight · {item_count} item{s} \
         awaiting your act</p>\n{sections}</section>\n",
        inquiries = groups.len(),
        ies = if groups.len() == 1 { "y" } else { "ies" },
        s = if item_count == 1 { "" } else { "s" },
    )
}

/// One inquiry's attention strip — used inside the board and atop the
/// channel page (where the heading already names the channel).
pub fn attention_group(group: &AttentionGroup, back: &str, titled: bool) -> String {
    let mut items = String::new();
    for item in &group.items {
        let _ = writeln!(items, "{}", attention_item(item, &group.channel, back));
    }
    let title = if titled {
        let display_name = group
            .name
            .clone()
            .unwrap_or_else(|| group.channel.to_string());
        format!(
            "<h3 class=\"attn-chan\"><a href=\"/channels/{href}\">{name}</a> \
             <span class=\"count\">{count} need{s} you</span></h3>\n",
            href = escape_html(&display_name),
            name = escape_html(&display_name),
            count = group.items.len(),
            s = if group.items.len() == 1 { "s" } else { "" },
        )
    } else {
        String::new()
    };
    format!("<section class=\"attn-group\">{title}{items}</section>")
}

/// One act awaiting the member: what it is, who it keeps waiting and for how
/// long (`docs/attention.md` — blocking-whom, by name), the content, and the
/// act inline.
fn attention_item(item: &crate::host::AttentionItem, channel: &ChannelId, back: &str) -> String {
    let entry = &item.entry;
    let waiting = wait_time(entry.timestamp.as_millis())
        .unwrap_or_else(|| format!("since {}", iso_utc(entry.timestamp.as_millis())));
    let (kind, badge, blocking, text, rationale, accept, decline) =
        match (&item.kind, &entry.payload) {
            (
                AttentionKind::Gate,
                EntryPayload::Proposal {
                    action, rationale, ..
                },
            ) => (
                "proposal",
                "pending",
                format!(
                    "<div class=\"blocking\">blocking <b>{}</b> for {waiting}</div>",
                    escape_html(&entry.author.display_name)
                ),
                action.clone(),
                rationale.clone(),
                "approve",
                "reject",
            ),
            (
                _,
                EntryPayload::Assertion {
                    statement,
                    rationale,
                    ..
                },
            ) => (
                "assertion",
                "provisional",
                format!(
                    "<div class=\"blocking quiet-block\">{} · unverified for {waiting}</div>",
                    escape_html(&entry.author.display_name)
                ),
                statement.clone(),
                rationale.clone(),
                "ratify",
                "park",
            ),
            // The host only emits the two shapes above; render anything else inert.
            _ => return String::new(),
        };
    format!(
        "<article class=\"card attn\">\
         <header><span class=\"kind\">{kind}</span>\
         <span class=\"badge {badge}\">{badge}</span>\
         <span class=\"spacer\"></span>\
         <span class=\"when\">{when}</span></header>\
         {blocking}\
         <div class=\"statement clamp\">{text}</div>\
         <details class=\"why\"><summary>why</summary><p>{why}</p></details>\
         {form}</article>",
        when = escape_html(&iso_utc(entry.timestamp.as_millis())),
        text = escape_html(&text),
        why = escape_html(&rationale),
        form = act_forms_with_frame(entry, channel, accept, decline, back),
    )
}

/// The human-facing channel page: the projected ledger as entry cards, with
/// rationale and provenance visible (collapsible) and verification forms
/// (ratify/park on provisional assertions, approve/reject on pending
/// proposals) — the human write surface. Forms post id-addressed URLs (ids
/// are URL-safe; names may not be) and require a rationale.
///
/// `nav` feeds the sidebar; pass `&[]` where navigation is irrelevant.
/// `substrate` is this channel's home substrate, prefilled (hidden) into the
/// contextual open-an-inquiry form.
pub fn channel_html(
    nav: &[ChannelSummary],
    name: &str,
    id: &ChannelId,
    view: &ChannelView,
    substrate: &std::path::Path,
    workspace: Option<&std::path::Path>,
) -> String {
    let mut cards = String::new();
    for entry in &view.entries {
        let _ = writeln!(cards, "{}", entry_card(entry, view, id));
    }
    let body = if view.entries.is_empty() {
        "<p class=\"empty\">(no entries)</p>".to_string()
    } else {
        cards
    };
    let party = if view.party.is_empty() {
        String::new()
    } else {
        let chips: Vec<String> = view
            .party
            .iter()
            .map(|member| {
                let marker = match member.kind {
                    MemberKind::Human => "",
                    MemberKind::Agent => " · agent",
                };
                format!(
                    "<span class=\"chip\" title=\"{email}\">{name}{marker}</span>",
                    email = escape_html(&member.email),
                    name = escape_html(&member.display_name),
                )
            })
            .collect();
        format!(
            "<div class=\"party\" title=\"founder first; only members' entries count \
             (docs/adr/0017)\">{}</div>\n",
            chips.join("")
        )
    };
    // The channel's own attention strip: what here awaits the member, above
    // the full ledger (docs/attention.md).
    let strip_group = crate::host::attention_for_view(id, view);
    let strip = if strip_group.items.is_empty() {
        String::new()
    } else {
        format!(
            "<section class=\"board\"><h2 class=\"board-head\">needs you here</h2>\n{}</section>\n",
            attention_group(&strip_group, &format!("/channels/{id}"), false)
        )
    };
    // Start work (docs/adr/0023): intent + workspace (prefilled once
    // remembered), spawning a real harness session. Hidden on closed
    // channels — reopen first.
    let start_work = if view.closed {
        String::new()
    } else {
        // A backend suggestion (e.g. install WSL to stop console windows
        // flashing) when the harness fell back to native (docs/adr/0023).
        let hint = match crate::launch::harness_hint() {
            Some(text) => format!("<p class=\"meta hint\">⚠ {}</p>", escape_html(text)),
            None => String::new(),
        };
        format!(
            "<section class=\"board\" id=\"start-work\">\
             <h2 class=\"board-head\">start work</h2>\n\
             <form class=\"act open-channel\" method=\"post\" action=\"/channels/{id}/sessions\">\
             <input name=\"intent\" placeholder=\"what should the agent do? e.g. fix the flaky \
             sync test\" required>\
             <input name=\"workspace\" value=\"{workspace}\" placeholder=\"workspace repo path \
             (remembered after first launch)\"{ws_required}>\
             <button class=\"primary\">launch</button>\
             </form>\
             <p class=\"meta\">spawns Claude Code headless in the workspace \
             (docs/adr/0023); progress lands below as the session's state and artifacts</p>\
             {hint}</section>\n",
            workspace = escape_html(
                &workspace
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ),
            ws_required = if workspace.is_some() { "" } else { " required" },
        )
    };
    let sessions = sessions_section(view, id);
    // The human brief: the same scaled shape the agent brief carries
    // (state, not history), rendered as the page — with the full transcript
    // collapsed below instead of *being* the page.
    let shape = brief_shape(view);
    let notes = act_notes(view);
    let standing = standing_decisions_section(&shape, &notes);
    let recently = recently_section(view);
    let folded: usize = notes.values().map(Vec::len).sum();
    let footer = format!(
        "<p class=\"meta\">{folded} verification acts folded into the lines above · \
         {superseded} superseded entries collapsed into their corrections · \
         {parked} parked dead-ends and {rejected} rejected proposals in the full \
         ledger below</p>\n",
        superseded = shape.superseded,
        parked = shape.parked.len(),
        rejected = shape.rejected.len(),
    );
    // No picker, no decision: a sibling inquiry opens in this channel's own
    // home substrate (a storage fact the form carries, not a choice).
    let open_here = format!(
        "<section class=\"board\"><h2 class=\"board-head\">open an inquiry here</h2>\n\
         <form class=\"act open-channel\" method=\"post\" action=\"/channels\">\
         <input type=\"hidden\" name=\"repo\" value=\"{repo}\">\
         <input name=\"name\" placeholder=\"a name for a new unit of inquiry in \
         {repo_label}\" required>\
         <button class=\"primary\">open</button>\
         </form></section>",
        repo = escape_html(&substrate.display().to_string()),
        repo_label = escape_html(
            &substrate
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| substrate.display().to_string())
        ),
    );
    // Rename and close: lifecycle acts (docs/adr/0016/0022) — collapsed
    // behind the title so they never compete with the brief. A closed
    // channel leads with the banner and offers reopen instead.
    let rename = format!(
        "<details class=\"rename\"><summary>rename this channel</summary>\
         <form class=\"act\" method=\"post\" action=\"/channels/{id}/rename\">\
         <input name=\"name\" value=\"{name}\" required>\
         <input name=\"rationale\" placeholder=\"why — a rationale, not a checkbox\" required>\
         <button class=\"primary\">rename</button>\
         </form></details>\n",
        name = escape_html(name),
    );
    let lifecycle = if view.closed {
        format!(
            "<div class=\"closed-banner\">this channel is <strong>closed</strong> — the \
             record remains, the inquiry left the working set (docs/adr/0022)</div>\n\
             <details class=\"rename\"><summary>reopen this channel</summary>\
             <form class=\"act\" method=\"post\" action=\"/channels/{id}/reopen\">\
             <input name=\"rationale\" placeholder=\"why it resumes — a rationale, not a \
             checkbox\" required>\
             <button class=\"primary\">reopen</button>\
             </form></details>\n"
        )
    } else {
        format!(
            "<details class=\"rename\"><summary>close this channel</summary>\
             <form class=\"act\" method=\"post\" action=\"/channels/{id}/close\">\
             <input name=\"rationale\" placeholder=\"why it closes — outcome reached, \
             superseded, abandoned…\" required>\
             <button class=\"primary\">close</button>\
             </form></details>\n"
        )
    };
    let content = format!(
        "<h1>{name}</h1>\n\
         <p class=\"meta\">channel {id} · {count} entries · read-only projection</p>\n\
         {rename}{lifecycle}{party}{strip}{start_work}{sessions}{standing}{recently}{footer}\
         <details class=\"ledger\"><summary class=\"board-head\">the full ledger \
         ({count} entries)</summary>\n{body}</details>\n{open_here}",
        name = escape_html(name),
        count = view.entries.len(),
    );
    page_shell(&format!("junto — {name}"), nav, Some(id), &content)
}

/// The human rendering of the brief's "standing decisions" tier: recent
/// ratified decisions in full, older ones clamped to their first line, the
/// oldest a count — each with who ratified it. Mirrors [`brief_markdown`]'s
/// tiers exactly; the full bodies live in the collapsed ledger below.
fn standing_decisions_section(
    shape: &BriefShape<'_>,
    notes: &std::collections::HashMap<EntryId, Vec<ActNote<'_>>>,
) -> String {
    if shape.ratified.is_empty() {
        return String::new();
    }
    let mut items = String::new();
    for (index, entry) in shape.ratified.iter().rev().enumerate() {
        if index >= BRIEF_RECENT_FULL + BRIEF_OLDER_CLAMPED {
            let _ = writeln!(
                items,
                "<li class=\"older\">…and {} older standing decisions (in the full \
                 ledger below)</li>",
                shape.ratified.len() - index
            );
            break;
        }
        let (text, corrects) = match &entry.payload {
            EntryPayload::Assertion { statement, .. } => (statement.as_str(), String::new()),
            EntryPayload::Correction {
                target, statement, ..
            } => (
                statement.as_str(),
                format!(
                    " <span class=\"by\">(correction of <code>{}</code>)</span>",
                    short(target)
                ),
            ),
            _ => continue,
        };
        let body = if index < BRIEF_RECENT_FULL {
            text.to_string()
        } else {
            clamp(text, BRIEF_CLAMP)
        };
        let by = last_act(notes, &entry.id, "ratified")
            .map(|note| {
                format!(
                    " <span class=\"by\">— ratified by {}</span>",
                    escape_html(&note.author.display_name)
                )
            })
            .unwrap_or_default();
        let _ = writeln!(
            items,
            "<li><code>{id}</code>{corrects} {body}{by}</li>",
            id = short(&entry.id),
            body = escape_html(&body),
        );
    }
    format!(
        "<section class=\"board\"><h2 class=\"board-head\">standing decisions \
         (newest first)</h2>\n<ul class=\"standing\">{items}</ul></section>\n"
    )
}

/// The human rendering of the brief's "recently" tail: the last few entries
/// as one-liners, for resumption — what just happened here.
fn recently_section(view: &ChannelView) -> String {
    if view.entries.is_empty() {
        return String::new();
    }
    let mut items = String::new();
    let tail_start = view.entries.len().saturating_sub(BRIEF_RECENT_TAIL);
    for entry in view.entries[tail_start..].iter().rev() {
        let _ = writeln!(
            items,
            "<li><span class=\"when\">{when}</span> {who} {line}</li>",
            when = escape_html(&iso_utc(entry.timestamp.as_millis())),
            who = escape_html(&entry.author.display_name),
            line = backticks_to_code(&recent_line(entry)),
        );
    }
    format!(
        "<section class=\"board\"><h2 class=\"board-head\">recently</h2>\n\
         <ul class=\"standing\">{items}</ul></section>\n"
    )
}

/// Escape text for HTML, rendering markdown-style `backtick` spans as
/// `<code>` — the brief's one-liners are written for both surfaces.
fn backticks_to_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (index, piece) in text.split('`').enumerate() {
        if index % 2 == 1 && !piece.is_empty() {
            out.push_str("<code>");
            out.push_str(&escape_html(piece));
            out.push_str("</code>");
        } else {
            out.push_str(&escape_html(piece));
        }
    }
    out
}

/// The visual family of an entry kind, so the ledger scans by what an entry
/// *is*: a decision to weigh (blue), agent work and its outputs (teal), or a
/// verification/lifecycle act (quiet). Purely presentational — the families
/// echo the kernel's subject/act split plus the session family of
/// `docs/adr/0020`.
fn entry_family(payload: &EntryPayload) -> &'static str {
    match payload {
        EntryPayload::Assertion { .. } | EntryPayload::Proposal { .. } => "fam-decision",
        EntryPayload::SessionStarted { .. }
        | EntryPayload::SessionUpdated { .. }
        | EntryPayload::ArtifactAttached { .. } => "fam-work",
        EntryPayload::ChannelOpened { .. }
        | EntryPayload::MemberAdded { .. }
        | EntryPayload::ChannelClosed { .. }
        | EntryPayload::ChannelReopened { .. }
        | EntryPayload::Ratification { .. }
        | EntryPayload::Park { .. }
        | EntryPayload::Correction { .. }
        | EntryPayload::Approval { .. }
        | EntryPayload::Rejection { .. } => "fam-act",
    }
}

/// One ledger entry as a card: kind chip + state badge + author/when header,
/// the content itself, and the rationale/provenance the page used to hide.
fn entry_card(entry: &LedgerEntry, view: &ChannelView, channel: &ChannelId) -> String {
    let unrecognized = view.unrecognized.contains(&entry.id);
    let (kind, badge, statement, rationale, provenance, target) = match &entry.payload {
        EntryPayload::ChannelOpened { name } => (
            "genesis",
            None,
            Some(format!("channel '{name}' opened")),
            None,
            None,
            None,
        ),
        EntryPayload::MemberAdded { member } => (
            "member added",
            None,
            Some(member_label(member)),
            None,
            None,
            None,
        ),
        EntryPayload::ChannelClosed { rationale } => (
            "channel closed",
            None,
            None,
            Some(rationale.as_str()),
            None,
            None,
        ),
        EntryPayload::ChannelReopened { rationale } => (
            "channel reopened",
            None,
            None,
            Some(rationale.as_str()),
            None,
            None,
        ),
        EntryPayload::Assertion {
            statement,
            rationale,
            provenance,
            ..
        } => (
            "assertion",
            Some(standing_label(view, entry)),
            Some(statement.clone()),
            Some(rationale.as_str()),
            Some(provenance.as_slice()),
            None,
        ),
        EntryPayload::Ratification { target, rationale } => (
            "ratification",
            None,
            None,
            Some(rationale.as_str()),
            None,
            Some(*target),
        ),
        EntryPayload::Park { target, rationale } => (
            "park",
            None,
            None,
            Some(rationale.as_str()),
            None,
            Some(*target),
        ),
        EntryPayload::Correction {
            target,
            statement,
            rationale,
        } => (
            "correction",
            None,
            Some(statement.clone()),
            Some(rationale.as_str()),
            None,
            Some(*target),
        ),
        EntryPayload::Proposal {
            action,
            rationale,
            provenance,
            ..
        } => (
            "proposal",
            Some(gate_label(view, entry)),
            Some(action.clone()),
            Some(rationale.as_str()),
            Some(provenance.as_slice()),
            None,
        ),
        EntryPayload::Approval { target, rationale } => (
            "approval",
            None,
            None,
            Some(rationale.as_str()),
            None,
            Some(*target),
        ),
        EntryPayload::Rejection { target, rationale } => (
            "rejection",
            None,
            None,
            Some(rationale.as_str()),
            None,
            Some(*target),
        ),
        EntryPayload::SessionStarted { intent } => (
            "agent session",
            Some(session_label(view, entry)),
            Some(intent.clone()),
            None,
            None,
            None,
        ),
        EntryPayload::SessionUpdated {
            target,
            state,
            note,
        } => (
            "session update",
            Some(session_state_label(*state)),
            None,
            Some(note.as_str()),
            None,
            Some(*target),
        ),
        EntryPayload::ArtifactAttached {
            target,
            kind,
            description,
            provenance,
        } => (
            "artifact",
            None,
            Some(format!("({kind}) {description}")),
            None,
            Some(provenance.as_slice()),
            Some(*target),
        ),
    };

    let badge = badge
        .map(|label| format!("<span class=\"badge {label}\">{label}</span>"))
        .unwrap_or_default();
    let unrecognized_badge = if unrecognized {
        "<span class=\"badge unrecognized\" title=\"author is not in the party; excluded \
         from standings and gates (docs/adr/0017)\">unrecognized</span>"
    } else {
        ""
    };
    let statement = statement
        .map(|text| format!("<div class=\"statement\">{}</div>", escape_html(&text)))
        .unwrap_or_default();
    let target = target
        .map(|target| format!("<div class=\"target\">acts on <code>{target}</code></div>",))
        .unwrap_or_default();
    let rationale = rationale
        .map(|text| {
            format!(
                "<details class=\"why\"><summary>why</summary>\
                 <p>{}</p></details>",
                escape_html(text)
            )
        })
        .unwrap_or_default();
    let provenance = provenance
        .filter(|refs| !refs.is_empty())
        .map(provenance_details)
        .unwrap_or_default();

    format!(
        "<article class=\"card {family}{flag}\">\
         <header><span class=\"kind\">{kind}</span>{badge}{unrecognized_badge}\
         <span class=\"spacer\"></span>\
         <span class=\"who\" title=\"{email}\">{who}</span>\
         <span class=\"when\">{when}</span></header>\
         {statement}{target}{rationale}{provenance}\
         <footer class=\"id\">{id}</footer>\
         {form}</article>",
        family = entry_family(&entry.payload),
        flag = if unrecognized { " flagged" } else { "" },
        email = escape_html(&entry.author.email),
        who = escape_html(&entry.author.display_name),
        when = escape_html(&iso_utc(entry.timestamp.as_millis())),
        id = entry.id,
        form = verification_form(entry, view, channel),
    )
}

/// The "agent sessions" board: one card per session, newest first — state
/// badge, intent, the artifacts it produced (the verifiable outputs a human
/// reviews instead of scrollback), and a steer box once the turn has landed
/// (`docs/adr/0023`: steering is between turns). Empty when the channel has
/// none.
/// Client wiring for agent sessions (`docs/adr/0023`), two parts:
/// - **Live feeds:** each running `.live` box opens an `EventSource`, appends a
///   structured progress line per `live` event, and reloads to the persisted
///   outcome on `end`. Read-only — steering stays a separate recorded POST.
/// - **Inline output:** each `details.artifact` lazy-loads its full body (the
///   memo/diff) from the artifact endpoint the first time it's open, so the
///   agent's output reads inline as a stream instead of a snippet + link.
///
/// All text is set via `textContent`, so agent output can never inject markup
/// (a feed/stream, not scrollback).
const SESSIONS_SCRIPT: &str = r#"<script>
(function(){
  document.querySelectorAll('.live').forEach(function(box){
    if(box.dataset.wired) return; box.dataset.wired='1';
    var feed=box.querySelector('.live-feed');
    var url='/channels/'+box.dataset.channel+'/sessions/'+box.dataset.session+'/stream';
    var es=new EventSource(url);
    var marks={tool:'⚙ ',status:'· ',result:'✓ ',error:'✗ '};
    es.addEventListener('live',function(e){
      var ev; try{ev=JSON.parse(e.data);}catch(_){return;}
      var li=document.createElement('li');
      li.className='le le-'+(ev.kind||'status');
      li.textContent=(marks[ev.kind]||'')+(ev.text||'');
      feed.appendChild(li);
      feed.scrollTop=feed.scrollHeight;
    });
    es.addEventListener('end',function(){ es.close(); location.reload(); });
  });
  function loadBody(d){
    var body=d.querySelector('.artifact-body');
    if(!body||body.dataset.loaded) return; body.dataset.loaded='1';
    fetch(body.dataset.src).then(function(r){return r.text();})
      .then(function(t){
        // Memos and diffs arrive as server-rendered (sanitized) HTML;
        // everything else is raw text set safely via textContent.
        if(body.dataset.format==='html'){ body.innerHTML=t; }
        else { body.textContent=t; }
      })
      .catch(function(){ body.textContent='(could not load artifact)'; });
  }
  document.querySelectorAll('details.artifact').forEach(function(d){
    if(d.open) loadBody(d);
    d.addEventListener('toggle',function(){ if(d.open) loadBody(d); });
  });
})();
</script>"#;

fn sessions_section(view: &ChannelView, channel: &ChannelId) -> String {
    // Sessions render newest-first; entries are already canonical, so walk
    // them in reverse and pick the session subjects.
    let mut cards = String::new();
    for entry in view.entries.iter().rev() {
        let EntryPayload::SessionStarted { intent } = &entry.payload else {
            continue;
        };
        let Some(session) = view.session(&entry.id) else {
            continue; // unrecognized author: the card stays in the ledger list
        };
        let state = session_state_label(session.state);
        // Steering targets a finished turn (--resume runs a new one); a
        // mid-turn session shows its liveness instead.
        let steer = match session.state {
            SessionState::Done | SessionState::Error => format!(
                "<form class=\"act\" method=\"post\" \
                 action=\"/channels/{channel}/sessions/{session_id}/steer\">\
                 <input name=\"message\" placeholder=\"steer — what should it do next?\" \
                 required>\
                 <button class=\"primary\">send</button>\
                 </form>",
                session_id = entry.id,
            ),
            SessionState::Working => format!(
                "<div class=\"live\" data-channel=\"{channel}\" data-session=\"{session_id}\">\
                 <p class=\"live-status\">running — live progress</p>\
                 <ul class=\"live-feed\"></ul></div>",
                session_id = entry.id,
            ),
            _ => String::new(),
        };
        let mut artifacts = String::new();
        for artifact_id in &session.artifacts {
            let Some(artifact) = view.entries.iter().find(|e| e.id == *artifact_id) else {
                continue;
            };
            let EntryPayload::ArtifactAttached {
                kind,
                description,
                provenance,
                ..
            } = &artifact.payload
            else {
                continue;
            };
            // The agent's output reads inline as a stream: the memo expands
            // open by default; bulkier artifacts (the diff) start collapsed.
            // The full body lazy-loads from the artifact endpoint — content
            // lives machine-local, never the ledger (docs/adr/0020/0023).
            if provenance.is_empty() {
                let _ = writeln!(
                    artifacts,
                    "<div class=\"artifact-note\"><span class=\"kind\">{kind}</span> \
                     {description}</div>",
                    kind = escape_html(kind),
                    description = escape_html(description),
                );
                continue;
            }
            // The memo renders as markdown and opens by default (the agent's
            // output as a stream); a diff renders with coloured lines and
            // starts collapsed (bulky); anything else is verbatim text. All
            // lazy-load their body from the artifact endpoint.
            let format = artifact_format(kind);
            let open = if format == ArtifactFormat::Markdown {
                " open"
            } else {
                ""
            };
            let src = format!("/channels/{channel}/artifacts/{}", artifact.id);
            let body = match format {
                ArtifactFormat::Markdown => format!(
                    "<div class=\"artifact-body md\" data-format=\"html\" \
                     data-src=\"{src}\">loading…</div>"
                ),
                ArtifactFormat::Diff => format!(
                    "<div class=\"artifact-body diff\" data-format=\"html\" \
                     data-src=\"{src}\">loading…</div>"
                ),
                ArtifactFormat::Raw => format!(
                    "<pre class=\"artifact-body\" data-format=\"text\" \
                     data-src=\"{src}\">loading…</pre>"
                ),
            };
            let _ = writeln!(
                artifacts,
                "<details class=\"artifact\"{open}>\
                 <summary><span class=\"kind\">{kind}</span> {description}</summary>\
                 {body}</details>",
                kind = escape_html(kind),
                description = escape_html(description),
            );
        }
        let artifacts = if artifacts.is_empty() {
            String::new()
        } else {
            format!("<div class=\"artifacts\">{artifacts}</div>")
        };
        let _ = writeln!(
            cards,
            "<article class=\"card fam-work\">\
             <header><span class=\"kind\">agent session</span>\
             <span class=\"badge {state}\">{state}</span>\
             <span class=\"spacer\"></span>\
             <span class=\"who\" title=\"{email}\">{who}</span>\
             <span class=\"when\">{when}</span></header>\
             <div class=\"statement\">{intent}</div>\
             {artifacts}{steer}\
             <footer class=\"id\">{id}</footer></article>",
            email = escape_html(&entry.author.email),
            who = escape_html(&entry.author.display_name),
            when = escape_html(&iso_utc(entry.timestamp.as_millis())),
            intent = escape_html(intent),
            id = entry.id,
        );
    }
    if cards.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "<section class=\"board\"><h2 class=\"board-head\">agent sessions</h2>\n\
         {cards}</section>\n"
    );
    // Wire live feeds (running sessions) and inline artifact loading (done
    // sessions) — the script no-ops for whichever isn't present.
    out.push_str(SESSIONS_SCRIPT);
    out
}

/// How an artifact's content is presented on the human surface.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArtifactFormat {
    /// The agent's prose memo — rendered as CommonMark.
    Markdown,
    /// A unified diff — rendered with per-line add/remove/hunk colouring.
    Diff,
    /// Anything else (a log, a chart dump) — shown verbatim in a `<pre>`.
    Raw,
}

/// The presentation for an artifact kind. Markdown and diff render to HTML;
/// the rest stay verbatim. Two concrete formatted kinds today (`memo`,
/// `diff`) — a playbook's own kinds fall through to raw.
pub fn artifact_format(kind: &str) -> ArtifactFormat {
    match kind {
        "memo" => ArtifactFormat::Markdown,
        "diff" => ArtifactFormat::Diff,
        _ => ArtifactFormat::Raw,
    }
}

/// Render a unified diff to HTML, one coloured line per row: additions green,
/// removals red, hunk headers and file/metadata lines tinted. Pure text → no
/// markup can leak (every line is escaped); the kernel never sees this.
pub fn render_diff(diff: &str) -> String {
    let mut out = String::new();
    for line in diff.lines() {
        let class = if line.starts_with("@@") {
            "d-hunk"
        } else if line.starts_with("+++")
            || line.starts_with("---")
            || line.starts_with("diff ")
            || line.starts_with("index ")
            || line.starts_with("new file")
            || line.starts_with("deleted file")
            || line.starts_with("rename ")
            || line.starts_with("similarity ")
            || line.starts_with("old mode")
            || line.starts_with("new mode")
        {
            "d-meta"
        } else if line.starts_with('+') {
            "d-add"
        } else if line.starts_with('-') {
            "d-del"
        } else {
            "d-ctx"
        };
        let _ = writeln!(out, "<div class=\"dl {class}\">{}</div>", escape_html(line));
    }
    out
}

/// Render an agent memo (CommonMark) to HTML for inline display. Agent output
/// is **untrusted**: raw HTML embedded in the markdown is neutralized to text
/// (never injected as markup), so a memo can format itself but never inject
/// script. The kernel never sees this — it's a render of machine-local
/// artifact content (`docs/adr/0020`/`0023`).
pub fn render_markdown(markdown: &str) -> String {
    use pulldown_cmark::{Event, Options, Parser, html};

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    let events = Parser::new_ext(markdown, options).map(|event| match event {
        // Escape any raw HTML the agent emitted instead of passing it through.
        Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
        other => other,
    });
    let mut html_out = String::new();
    html::push_html(&mut html_out, events);
    html_out
}

/// The provenance list, collapsed by default; http(s) URIs become links.
fn provenance_details(refs: &[ProvenanceRef]) -> String {
    let mut items = String::new();
    for provenance_ref in refs {
        let uri = provenance_ref.uri.as_str();
        let escaped = escape_html(uri);
        let item = if uri.starts_with("http://") || uri.starts_with("https://") {
            format!("<a href=\"{escaped}\" target=\"_blank\" rel=\"noreferrer\">{escaped}</a>")
        } else {
            format!("<code>{escaped}</code>")
        };
        let _ = writeln!(items, "<li>{item}</li>");
    }
    format!(
        "<details class=\"prov\"><summary>provenance ({count})</summary>\
         <ul>{items}</ul></details>",
        count = refs.len(),
    )
}

/// The verification form for one entry, when its derived state awaits one:
/// ratify/park for a provisional assertion, approve/reject for a pending
/// proposal; empty otherwise. One rationale input feeds whichever button is
/// pressed (`formaction` routes the second act).
fn verification_form(entry: &LedgerEntry, view: &ChannelView, channel: &ChannelId) -> String {
    let acts = match &entry.payload {
        EntryPayload::Assertion { .. }
            if matches!(view.standing(&entry.id), Some(Standing::Provisional)) =>
        {
            Some(("ratify", "park"))
        }
        EntryPayload::Proposal { .. }
            if matches!(view.gate_status(&entry.id), Some(GateStatus::Pending)) =>
        {
            Some(("approve", "reject"))
        }
        _ => None,
    };
    let Some((accept, decline)) = acts else {
        return String::new();
    };
    act_forms_with_frame(
        entry,
        channel,
        accept,
        decline,
        &format!("/channels/{channel}"),
    )
}

/// The act forms for one pending/provisional entry: the decision frame's
/// one-click options first (each with its drafted, editable rationale —
/// `docs/adr/0019`), then the plain free-text form.
fn act_forms_with_frame(
    entry: &LedgerEntry,
    channel: &ChannelId,
    accept: &str,
    decline: &str,
    back: &str,
) -> String {
    let frame = match &entry.payload {
        EntryPayload::Assertion { frame, .. } | EntryPayload::Proposal { frame, .. } => {
            frame.as_ref()
        }
        _ => None,
    };
    let mut out = String::new();
    if let Some(frame) = frame {
        for option in &frame.options {
            let route = frame_act_route(option.act);
            // Render only options coherent with this entry's pending acts —
            // incoherent frames are refused at record time, but synced
            // entries are trusted no further than rendering.
            if route != accept && route != decline {
                continue;
            }
            let _ = write!(
                out,
                "<form class=\"act option\" method=\"post\" \
                 action=\"/channels/{channel}/entries/{entry_id}/{route}\">\
                 <input type=\"hidden\" name=\"back\" value=\"{back}\">\
                 <button class=\"primary\">{label}</button>\
                 <input name=\"rationale\" value=\"{draft}\" required \
                 title=\"the drafted rationale — edit before choosing if it isn't quite yours\">\
                 </form>",
                entry_id = entry.id,
                back = escape_html(back),
                label = escape_html(&option.label),
                draft = escape_html(&option.rationale),
            );
        }
    }
    out.push_str(&act_form(entry.id, channel, accept, decline, back));
    out
}

/// The act form itself: one rationale input feeding whichever button is
/// pressed, and the return path the route redirects to afterwards. No member
/// code: the host derives the author from git config and authorizes
/// membership itself (the codes guard the agent-facing MCP surface, where
/// identity is claimed — not the human surface, where the host claims it).
fn act_form(
    entry_id: junto_kernel::EntryId,
    channel: &ChannelId,
    accept: &str,
    decline: &str,
    back: &str,
) -> String {
    format!(
        "<form class=\"act\" method=\"post\" \
         action=\"/channels/{channel}/entries/{entry_id}/{accept}\">\
         <input type=\"hidden\" name=\"back\" value=\"{back}\">\
         <input name=\"rationale\" placeholder=\"why — a rationale, not a checkbox\" required>\
         <button class=\"primary\">{accept}</button>\
         <button formaction=\"/channels/{channel}/entries/{entry_id}/{decline}\">{decline}</button>\
         </form>",
        back = escape_html(back),
    )
}

/// The one deliberate exception to the no-JS posture: a verification act is
/// a POST whose round trip includes git writes and a projection, long enough
/// that a silent button reads as a dead click. This inline script marks the
/// pressed act button "recording…" and disables the form's buttons — pure
/// progressive enhancement (the form submits identically without it), inline
/// and offline like everything else on the page. The disabling is deferred a
/// tick so it can never interfere with the browser collecting the form data.
const ACT_FEEDBACK_SCRIPT: &str = "<script>document.addEventListener('submit',function(e)\
{var f=e.target;if(!f.classList||!f.classList.contains('act'))return;\
var b=e.submitter;setTimeout(function(){f.classList.add('busy');\
f.querySelectorAll('button').forEach(function(x){x.disabled=true});\
if(b){b.textContent='recording\\u2026'}},0)});</script>";

/// The dark theme, keyed to the app icon palette (`docs/adr/0018`): one CSS
/// blob, no external assets — the pages must render identically in
/// the desktop shell's webview and a plain browser, offline. (JS: a single
/// inline act-feedback script, [`ACT_FEEDBACK_SCRIPT`] — nothing else.)
const CSS: &str = "\
:root{--bg:#11111b;--panel:#181825;--card:#1e1e2e;--border:#313244;--text:#cdd6f4;\
--muted:#7f849c;--soft:#a6adc8;--accent:#89b4fa;--green:#a6e3a1;--yellow:#f9e2af;\
--red:#f38ba8;--gray:#9399b2;--teal:#94e2d5}\
*{box-sizing:border-box}\
body{margin:0;background:var(--bg);color:var(--text);\
font:15px/1.55 system-ui,'Segoe UI',sans-serif}\
.app{display:flex;min-height:100vh}\
nav.side{width:232px;flex:none;background:var(--panel);border-right:1px solid var(--border);\
padding:1rem .75rem;position:sticky;top:0;height:100vh;overflow-y:auto}\
.brand{display:flex;align-items:center;gap:.5rem;font-weight:650;font-size:1.05rem;\
color:var(--text);text-decoration:none;margin:0 .25rem 1.1rem}\
.logo{display:inline-grid;place-items:center;width:1.6rem;height:1.6rem;border-radius:.45rem;\
background:var(--card);color:var(--accent);font-weight:700;border:1px solid var(--border)}\
.side-label{font-size:.68rem;text-transform:uppercase;letter-spacing:.08em;\
color:var(--muted);margin:0 .25rem .35rem}\
a.chan{display:flex;align-items:center;gap:.5rem;padding:.4rem .55rem;border-radius:.45rem;\
color:var(--soft);text-decoration:none;font-size:.9rem;margin-bottom:2px}\
a.chan:hover{background:var(--card);color:var(--text)}\
a.chan.active{background:var(--card);color:var(--text);outline:1px solid var(--border)}\
a.chan.open-link{color:var(--muted);font-size:.84rem;margin-top:.35rem}\
a.chan.open-link:hover{color:var(--accent)}\
.side-menu{margin-top:.5rem;font-size:.84rem}\
.side-menu>summary{cursor:pointer;color:var(--muted);user-select:none;\
padding:.3rem .55rem;border-radius:.45rem}\
.side-menu>summary:hover{color:var(--accent);background:var(--card)}\
.side-menu a.open-link{margin-top:0}\
.side-sub{font-size:.68rem;text-transform:uppercase;letter-spacing:.07em;\
color:var(--muted);margin:.7rem 0 .15rem;padding:0 .55rem}\
.closed-banner{color:var(--yellow);background:rgba(249,226,175,.08);\
border:1px solid rgba(249,226,175,.25);border-radius:.55rem;\
padding:.55rem .8rem;font-size:.88rem;margin:0 0 1rem}\
.chan-name{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.gatecount{flex:none;font-size:.7rem;font-weight:650;color:var(--bg);background:var(--yellow);\
border-radius:.6rem;padding:0 .45rem}\
main{flex:1;min-width:0;max-width:56rem;padding:2rem 2.25rem 4rem}\
h1{margin:0 0 .2rem;font-size:1.45rem}h2{margin:0;font-size:1.02rem}\
.meta{color:var(--muted);font-size:.82rem;margin:0 0 1.1rem}\
.empty{color:var(--muted)}\
.party{display:flex;flex-wrap:wrap;gap:.4rem;margin:0 0 1.2rem}\
.board{margin:0 0 1.6rem}\
.board-head{font-size:.78rem;text-transform:uppercase;letter-spacing:.08em;\
color:var(--muted);margin:1.4rem 0 .6rem}\
.all-clear{color:var(--green);font-size:.9rem}\
ul.standing{list-style:none;margin:0;padding:0;font-size:.9rem;line-height:1.55}\
ul.standing li{margin:.3rem 0;overflow-wrap:anywhere}\
ul.standing .by,ul.standing li.older{color:var(--muted)}\
details.ledger{margin-top:1.6rem}\
details.ledger>summary{cursor:pointer;user-select:none}\
details.ledger>summary:hover{color:var(--soft)}\
.attn-group{margin:0 0 1rem}\
.attn-chan{font-size:.95rem;margin:0 0 .45rem}\
.attn-chan a{color:var(--text);text-decoration:none}\
.attn-chan a:hover{color:var(--accent)}\
.count{font-size:.72rem;font-weight:600;color:var(--yellow);margin-left:.4rem}\
.card.attn{border-left:2px solid var(--yellow)}\
.blocking{color:var(--yellow);font-size:.84rem;margin-top:.5rem}\
.blocking.quiet-block{color:var(--muted)}\
.clamp{display:-webkit-box;-webkit-line-clamp:3;-webkit-box-orient:vertical;\
overflow:hidden}\
.chip{font-size:.74rem;color:var(--soft);border:1px solid var(--border);\
border-radius:.7rem;padding:.12rem .6rem;background:var(--panel)}\
.card{background:var(--card);border:1px solid var(--border);border-radius:.65rem;\
padding:.8rem .95rem;margin-bottom:.7rem}\
.card.flagged{border-color:var(--red)}\
.card.fam-decision{border-left:2px solid rgba(137,180,250,.55)}\
.card.fam-work{border-left:2px solid rgba(148,226,213,.55)}\
.card.fam-work .kind{color:var(--teal)}\
.card.fam-act .kind{color:var(--muted)}\
.cards{display:grid;grid-template-columns:repeat(auto-fill,minmax(min(24rem,100%),1fr));\
gap:.8rem}\
.cards>*{min-width:0}\
.chan-card{margin:0;padding:1rem 1.1rem;height:100%;min-width:0;\
transition:border-color .12s,transform .12s}\
.chan-card h2{font-size:1.12rem}\
.preview{color:var(--soft);font-size:.84rem;margin-top:.55rem;line-height:1.45;\
display:-webkit-box;-webkit-line-clamp:2;-webkit-box-orient:vertical;overflow:hidden}\
.attention{color:var(--yellow)}\
.card-link{text-decoration:none;color:inherit;display:block}\
.card-link:hover .card{border-color:var(--accent);transform:translateY(-1px)}\
.card header{display:flex;align-items:center;gap:.55rem;flex-wrap:wrap}\
.spacer{flex:1}\
.kind{font-size:.72rem;text-transform:uppercase;letter-spacing:.06em;color:var(--accent);\
font-weight:650}\
.badge{font-size:.7rem;font-weight:600;padding:.08rem .55rem;border-radius:.6rem;\
text-transform:uppercase;letter-spacing:.04em;border:1px solid transparent}\
.provisional,.pending,.awaiting-approval{color:var(--yellow);background:rgba(249,226,175,.12);\
border-color:rgba(249,226,175,.3)}\
.ratified,.approved,.done{color:var(--green);background:rgba(166,227,161,.12);\
border-color:rgba(166,227,161,.3)}\
.parked,.superseded,.quiet{color:var(--gray);background:rgba(147,153,178,.12);\
border-color:rgba(147,153,178,.3)}\
.rejected,.unrecognized,.error,.blocked{color:var(--red);background:rgba(243,139,168,.12);\
border-color:rgba(243,139,168,.3)}\
.working{color:var(--accent);background:rgba(137,180,250,.12);\
border-color:rgba(137,180,250,.3)}\
.who{color:var(--soft);font-size:.82rem}\
.when{color:var(--muted);font-size:.76rem}\
.statement{margin:.55rem 0 0;white-space:pre-wrap}\
.artifacts{margin:.6rem 0 0;font-size:.86rem}\
.artifact-note{color:var(--soft);margin:.3rem 0}\
details.artifact{margin:.45rem 0;border:1px solid var(--border);border-radius:.5rem;\
background:var(--panel);overflow:hidden}\
details.artifact>summary{cursor:pointer;user-select:none;padding:.45rem .7rem;color:var(--soft)}\
details.artifact>summary:hover{color:var(--text)}\
details.artifact[open]>summary{border-bottom:1px solid var(--border)}\
.live{margin:.55rem 0 0}\
.live-status{display:flex;align-items:center;gap:.45rem;color:var(--accent);font-size:.82rem;margin:0}\
.live-status::before{content:'';width:.5rem;height:.5rem;border-radius:50%;background:var(--accent);\
animation:livepulse 1.1s ease-in-out infinite}\
@keyframes livepulse{0%,100%{opacity:.3}50%{opacity:1}}\
.live-feed{list-style:none;margin:.5rem 0 0;padding:0;font-size:.84rem;line-height:1.5;\
max-height:18rem;overflow-y:auto;border-left:2px solid var(--border);padding-left:.7rem}\
.live-feed li{margin:.25rem 0;overflow-wrap:anywhere;white-space:pre-wrap}\
.le-assistant{color:var(--text)}\
.le-tool{color:var(--teal);font-family:ui-monospace,'Cascadia Mono',Consolas,monospace;font-size:.8rem}\
.le-status{color:var(--muted);font-size:.8rem}\
.le-result{color:var(--green)}\
.le-error{color:var(--red)}\
.meta-line{color:var(--muted);font-size:.8rem;margin-top:.45rem}\
.hint{color:var(--yellow);background:rgba(249,226,175,.07);border:1px solid rgba(249,226,175,.22);\
border-radius:.5rem;padding:.5rem .7rem;margin-top:.6rem}\
.artifact-body{margin:0;padding:.7rem .9rem;max-height:34rem;overflow:auto;\
overflow-wrap:anywhere;color:var(--soft)}\
pre.artifact-body{white-space:pre-wrap;font:.82rem/1.5 ui-monospace,'Cascadia Mono',Consolas,monospace}\
.artifact-body.md{font-size:.9rem;line-height:1.6;color:var(--text)}\
.artifact-body.md>:first-child{margin-top:0}\
.artifact-body.md>:last-child{margin-bottom:0}\
.artifact-body.md h1,.artifact-body.md h2,.artifact-body.md h3{line-height:1.3;margin:1.1rem 0 .5rem}\
.artifact-body.md h1{font-size:1.2rem}.artifact-body.md h2{font-size:1.08rem}\
.artifact-body.md h3{font-size:.98rem}\
.artifact-body.md p{margin:.55rem 0}\
.artifact-body.md ul,.artifact-body.md ol{margin:.5rem 0;padding-left:1.3rem}\
.artifact-body.md li{margin:.2rem 0}\
.artifact-body.md code{font:.85em ui-monospace,'Cascadia Mono',Consolas,monospace;\
background:var(--bg);padding:.06rem .3rem;border-radius:.3rem}\
.artifact-body.md pre{background:var(--bg);border:1px solid var(--border);border-radius:.45rem;\
padding:.7rem .85rem;overflow:auto}\
.artifact-body.md pre code{background:none;padding:0}\
.artifact-body.md a{color:var(--accent)}\
.artifact-body.md blockquote{margin:.5rem 0;padding-left:.8rem;border-left:2px solid var(--border);\
color:var(--soft)}\
.artifact-body.md table{border-collapse:collapse;margin:.5rem 0}\
.artifact-body.md th,.artifact-body.md td{border:1px solid var(--border);padding:.3rem .55rem}\
.artifact-body.diff{padding:.5rem 0;font:.82rem/1.45 ui-monospace,'Cascadia Mono',Consolas,monospace}\
.artifact-body.diff .dl{white-space:pre;padding:0 .9rem;min-height:1.25em}\
.d-add{background:rgba(166,227,161,.10);color:var(--green)}\
.d-del{background:rgba(243,139,168,.10);color:var(--red)}\
.d-hunk{color:var(--teal)}\
.d-meta{color:var(--muted)}\
.d-ctx{color:var(--soft)}\
.target{color:var(--muted);font-size:.82rem;margin-top:.5rem}\
code{font:.82em ui-monospace,'Cascadia Mono',Consolas,monospace;color:var(--soft);\
background:var(--panel);padding:.06rem .3rem;border-radius:.3rem}\
details{margin-top:.55rem;font-size:.86rem}\
details summary{cursor:pointer;color:var(--muted);user-select:none}\
details summary:hover{color:var(--soft)}\
details p{margin:.4rem 0 0;color:var(--soft);white-space:pre-wrap}\
details ul{margin:.4rem 0 0;padding-left:1.2rem;color:var(--soft)}\
details a{color:var(--accent)}\
footer.id{color:#45475a;font-size:.68rem;\
font-family:ui-monospace,'Cascadia Mono',Consolas,monospace;margin-top:.55rem;\
overflow-wrap:anywhere}\
.statement,.preview,.meta-line{overflow-wrap:anywhere}\
form.act{display:flex;gap:.45rem;flex-wrap:wrap;margin-top:.65rem;padding-top:.65rem;\
border-top:1px solid var(--border)}\
form.act input{flex:1;min-width:10rem;background:var(--bg);color:var(--text);\
border:1px solid var(--border);border-radius:.45rem;padding:.32rem .6rem;font-size:.84rem}\
form.act input:focus{outline:1px solid var(--accent)}\
form.act select{background:var(--bg);color:var(--text);border:1px solid var(--border);\
border-radius:.45rem;padding:.32rem .6rem;font-size:.84rem}\
form.act button{background:var(--panel);color:var(--soft);border:1px solid var(--border);\
border-radius:.45rem;padding:.32rem .85rem;font-size:.84rem;cursor:pointer}\
form.act button:hover{color:var(--text);border-color:var(--accent)}\
form.act button.primary{background:rgba(137,180,250,.15);color:var(--accent);\
border-color:rgba(137,180,250,.4)}\
form.act.option{border-top:0;padding-top:0;margin-top:.45rem}\
form.act.option button.primary{flex:none;min-width:7rem}\
form.act.option input[name=rationale]{color:var(--soft);font-style:italic}\
form.act.busy{opacity:.65}\
form.act button:disabled{cursor:wait}\
a.back-link{color:var(--muted);font-size:.84rem}\
a.back-link:hover{color:var(--soft)}";

/// Minimal HTML escaping for text interpolated into the page.
fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Epoch milliseconds → `YYYY-MM-DD HH:MM UTC` for the human page.
fn iso_utc(millis: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(millis.div_euclid(1000)) {
        Ok(dt) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02} UTC",
            dt.year(),
            u8::from(dt.month()),
            dt.day(),
            dt.hour(),
            dt.minute()
        ),
        Err(_) => format!("@{millis}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use junto_kernel::{EntryId, Member, Timestamp};
    use std::collections::HashMap;

    fn view_with(entries: Vec<LedgerEntry>) -> ChannelView {
        let mut standings = HashMap::new();
        let mut gate_status = HashMap::new();
        for e in &entries {
            match e.payload {
                EntryPayload::Assertion { .. } => {
                    standings.insert(e.id, Standing::Provisional);
                }
                EntryPayload::Proposal { .. } => {
                    gate_status.insert(e.id, GateStatus::Pending);
                }
                _ => {}
            }
        }
        ChannelView {
            name: None,
            entries,
            party: Vec::new(),
            unrecognized: std::collections::HashSet::new(),
            standings,
            gate_status,
            sessions: HashMap::new(),
            closed: false,
        }
    }

    fn assertion(statement: &str) -> LedgerEntry {
        LedgerEntry {
            id: EntryId::new(),
            channel: ChannelId::new(),
            author: Member::human("Ada", "ada@example.com"),
            timestamp: Timestamp::from_millis(1_781_046_734_154),
            payload: EntryPayload::Assertion {
                statement: statement.into(),
                rationale: "r".into(),
                provenance: vec![],
                frame: None,
            },
        }
    }

    /// A verification act entry by Dan, for fold-into-target tests.
    fn act(payload: EntryPayload) -> LedgerEntry {
        LedgerEntry {
            id: EntryId::new(),
            channel: ChannelId::new(),
            author: Member::human("Dan", "dan@example.com"),
            timestamp: Timestamp::from_millis(1_781_046_734_155),
            payload,
        }
    }

    #[test]
    fn artifact_formats_map_by_kind() {
        assert_eq!(artifact_format("memo"), ArtifactFormat::Markdown);
        assert_eq!(artifact_format("diff"), ArtifactFormat::Diff);
        assert_eq!(artifact_format("log"), ArtifactFormat::Raw);
    }

    #[test]
    fn diff_colouring_classifies_lines() {
        let html = render_diff("diff --git a/x b/x\n@@ -1,2 +1,2 @@\n context\n-removed\n+added\n");
        assert!(html.contains("d-meta"), "{html}");
        assert!(html.contains("d-hunk"), "{html}");
        assert!(
            html.contains("<div class=\"dl d-del\">-removed</div>"),
            "{html}"
        );
        assert!(
            html.contains("<div class=\"dl d-add\">+added</div>"),
            "{html}"
        );
    }

    #[test]
    fn markdown_renders_and_neutralizes_raw_html() {
        let html = render_markdown(
            "# Heading\n\nsome **bold** and `code`.\n\n- one\n- two\n\n\
             <script>alert('x')</script>",
        );
        assert!(html.contains("<h1>Heading</h1>"), "{html}");
        assert!(html.contains("<strong>bold</strong>"), "{html}");
        assert!(html.contains("<code>code</code>"), "{html}");
        assert!(html.contains("<li>one</li>"), "{html}");
        // Agent output is untrusted: raw HTML is escaped, never injected.
        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
    }

    #[test]
    fn channel_page_shows_sessions_with_their_artifacts() {
        let agent = Member::agent("Coder", "coder@junto.local");
        let session = LedgerEntry {
            id: EntryId::new(),
            channel: ChannelId::new(),
            author: agent.clone(),
            timestamp: Timestamp::from_millis(1_781_046_734_154),
            payload: EntryPayload::SessionStarted {
                intent: "fix the flaky sync test".into(),
            },
        };
        let artifact = LedgerEntry {
            id: EntryId::new(),
            channel: ChannelId::new(),
            author: agent,
            timestamp: Timestamp::from_millis(1_781_046_734_155),
            payload: EntryPayload::ArtifactAttached {
                target: session.id,
                kind: "diff".into(),
                description: "the fix as a unified diff".into(),
                provenance: vec![],
            },
        };
        let mut view = view_with(vec![session.clone(), artifact.clone()]);
        view.sessions.insert(
            session.id,
            junto_kernel::SessionView {
                state: SessionState::Blocked,
                artifacts: vec![artifact.id],
            },
        );

        let channel = ChannelId::new();
        let html = channel_html(
            &[],
            "t",
            &channel,
            &view,
            std::path::Path::new("/repo"),
            None,
        );
        assert!(html.contains("agent sessions"), "{html}");
        assert!(html.contains("fix the flaky sync test"), "{html}");
        assert!(html.contains("badge blocked"), "{html}");
        assert!(html.contains("the fix as a unified diff"), "{html}");
    }

    #[test]
    fn channel_page_is_a_brief_with_the_ledger_collapsed() {
        // The human brief mirrors the agent brief's shape: standing decisions
        // tiered with their ratifier, a "recently" tail, and the full ledger
        // demoted to a collapsed transcript instead of being the page.
        let decision = assertion("the settled claim");
        let ratify = act(EntryPayload::Ratification {
            target: decision.id,
            rationale: "verified".into(),
        });
        let mut view = view_with(vec![decision.clone(), ratify]);
        view.standings.insert(decision.id, Standing::Ratified);

        let html = channel_html(
            &[],
            "t",
            &ChannelId::new(),
            &view,
            std::path::Path::new("/repo"),
            None,
        );
        assert!(html.contains("standing decisions"), "{html}");
        assert!(html.contains("the settled claim"), "{html}");
        assert!(html.contains("ratified by Dan"), "{html}");
        assert!(html.contains("recently"), "{html}");
        assert!(
            html.contains("<details class=\"ledger\">"),
            "the transcript is collapsed: {html}"
        );
    }

    #[test]
    fn the_new_page_carries_both_forms_and_the_sidebar_offers_the_menu() {
        // One substrate: the open form posts with no picker (the host picks
        // it); the setup-repo form is there too.
        let one = new_html(&[], &[std::path::PathBuf::from("/repo/a")]);
        assert!(one.contains("action=\"/channels\""), "{one}");
        assert!(one.contains("name=\"name\""), "{one}");
        assert!(!one.contains("name=\"repo\""), "{one}");
        assert!(one.contains("action=\"/repos\""), "{one}");

        // Several substrates: a home-substrate picker appears.
        let many = new_html(
            &[],
            &[
                std::path::PathBuf::from("/repo/a"),
                std::path::PathBuf::from("/repo/b"),
            ],
        );
        assert!(many.contains("name=\"repo\""), "{many}");
        assert!(many.contains("/repo/b"), "{many}");

        // The index dropped the inline forms; every page's sidebar carries
        // the "+ new" menu instead.
        let index = index_html(&[], &[]);
        assert!(!index.contains("action=\"/repos\""), "{index}");
        assert!(index.contains("+ new"), "{index}");
        assert!(index.contains("href=\"/new#open-channel\""), "{index}");
        assert!(index.contains("href=\"/new#setup-repo\""), "{index}");
    }

    #[test]
    fn channel_page_offers_rename() {
        let view = view_with(vec![]);
        let channel = ChannelId::new();
        let html = channel_html(
            &[],
            "old-name",
            &channel,
            &view,
            std::path::Path::new("/r"),
            None,
        );
        assert!(html.contains("rename this channel"), "{html}");
        assert!(
            html.contains(&format!("/channels/{channel}/rename")),
            "{html}"
        );
        assert!(html.contains("value=\"old-name\""), "{html}");
    }

    #[test]
    fn sidebar_groups_channels_by_substrate() {
        let nav = vec![
            ChannelSummary {
                id: ChannelId::new(),
                name: Some("alpha".into()),
                substrate: std::path::PathBuf::from("/repo/one"),
                entry_count: 1,
                last_activity: None,
                open_gates: 0,
                members: 1,
                latest: None,
                closed: false,
            },
            ChannelSummary {
                id: ChannelId::new(),
                name: Some("beta".into()),
                substrate: std::path::PathBuf::from("/repo/two"),
                entry_count: 1,
                last_activity: None,
                open_gates: 0,
                members: 1,
                latest: None,
                closed: false,
            },
        ];
        let view = view_with(vec![]);
        let html = channel_html(
            &nav,
            "alpha",
            &nav[0].id,
            &view,
            std::path::Path::new("/r"),
            None,
        );
        assert!(html.contains("<div class=\"side-sub\""), "{html}");
        assert!(html.contains(">one</div>"), "{html}");
        assert!(html.contains(">two</div>"), "{html}");

        // A single-substrate host gets no group headings.
        let solo = vec![nav[0].clone()];
        let html = channel_html(
            &solo,
            "alpha",
            &solo[0].id,
            &view,
            std::path::Path::new("/r"),
            None,
        );
        assert!(!html.contains("<div class=\"side-sub\""), "{html}");
    }

    #[test]
    fn channel_page_offers_open_an_inquiry_here() {
        // The contextual form carries the channel's home substrate hidden —
        // a sibling inquiry opens in the same repo, no picker.
        let view = view_with(vec![]);
        let html = channel_html(
            &[],
            "t",
            &ChannelId::new(),
            &view,
            std::path::Path::new("/repo/wmux"),
            None,
        );
        assert!(html.contains("open an inquiry here"), "{html}");
        assert!(
            html.contains("type=\"hidden\" name=\"repo\" value=\"/repo/wmux\""),
            "{html}"
        );
    }

    #[test]
    fn cards_carry_their_entry_family() {
        // Decisions, work, and acts are visually distinct families: the card
        // class drives the chip color and edge tint, so a ledger scans by
        // what each entry *is*.
        let decision = assertion("a claim to weigh");
        let work = LedgerEntry {
            id: EntryId::new(),
            channel: ChannelId::new(),
            author: Member::agent("Coder", "coder@junto.local"),
            timestamp: Timestamp::from_millis(1_781_046_734_155),
            payload: EntryPayload::ArtifactAttached {
                target: decision.id, // dangling-by-kind is fine for rendering
                kind: "diff".into(),
                description: "an output to inspect".into(),
                provenance: vec![],
            },
        };
        let act = act(EntryPayload::Ratification {
            target: decision.id,
            rationale: "verified".into(),
        });
        let view = view_with(vec![decision, work, act]);
        let html = channel_html(
            &[],
            "t",
            &ChannelId::new(),
            &view,
            std::path::Path::new("/repo"),
            None,
        );
        assert!(html.contains("card fam-decision"), "{html}");
        assert!(html.contains("card fam-work"), "{html}");
        assert!(html.contains("card fam-act"), "{html}");
    }

    #[test]
    fn scaled_brief_folds_acts_and_decays_resolved() {
        let ratified = assertion("The sky is blue. Checked at noon.");
        let parked = assertion("Cold fusion works");
        let superseded = assertion("old text of the claim");
        let correction = LedgerEntry {
            id: EntryId::new(),
            channel: ChannelId::new(),
            author: Member::human("Ada", "ada@example.com"),
            timestamp: Timestamp::from_millis(1_781_046_734_154),
            payload: EntryPayload::Correction {
                target: superseded.id,
                statement: "new text of the claim".into(),
                rationale: "fixed".into(),
            },
        };
        let proposal = LedgerEntry {
            id: EntryId::new(),
            channel: ChannelId::new(),
            author: Member::human("Ada", "ada@example.com"),
            timestamp: Timestamp::from_millis(1_781_046_734_154),
            payload: EntryPayload::Proposal {
                action: "merge the fix".into(),
                rationale: "ready".into(),
                provenance: vec![],
                frame: None,
                requirement: junto_kernel::ApprovalRequirement::Count(1),
            },
        };
        let acts = vec![
            act(EntryPayload::Ratification {
                target: ratified.id,
                rationale: "verified".into(),
            }),
            act(EntryPayload::Park {
                target: parked.id,
                rationale: "dead end".into(),
            }),
            act(EntryPayload::Approval {
                target: proposal.id,
                rationale: "go".into(),
            }),
        ];
        let standings: HashMap<_, _> = [
            (ratified.id, Standing::Ratified),
            (parked.id, Standing::Parked),
            (superseded.id, Standing::Superseded),
        ]
        .into();
        let gate_status: HashMap<_, _> = [(proposal.id, GateStatus::Approved)].into();
        let mut entries = vec![parked, superseded, ratified, proposal, correction];
        entries.extend(acts);
        let view = ChannelView {
            name: Some("t".into()),
            entries,
            party: Vec::new(),
            unrecognized: Default::default(),
            standings,
            gate_status,
            sessions: Default::default(),
            closed: false,
        };
        let brief = brief_markdown("t", &ChannelId::new(), &view);

        // Acts fold into their targets instead of renting lines.
        assert!(brief.contains("ratified by Dan"), "{brief}");
        assert!(!brief.contains("ratification of"), "{brief}");
        // The superseded body collapses; the correction carries the live text.
        assert!(!brief.contains("old text of the claim"), "{brief}");
        assert!(brief.contains("new text of the claim"), "{brief}");
        assert!(brief.contains("(correction of"), "{brief}");
        // Parked dead-ends do not rent standing lines — counted, not shown.
        assert!(!brief.contains("Cold fusion"), "{brief}");
        assert!(brief.contains("1 parked dead-ends"), "{brief}");
        // Approved proposals appear as sanctioned actions with their approver.
        assert!(brief.contains("merge the fix"), "{brief}");
        assert!(brief.contains("approved by Dan"), "{brief}");
    }

    #[test]
    fn scaled_brief_tiers_standing_decisions() {
        // 30 ratified decisions: newest 10 render in full, the next 15 clamp
        // to the first sentence, the oldest 5 decay to a count.
        let entries: Vec<LedgerEntry> = (0..30)
            .map(|i| assertion(&format!("decision {i:02} settled. extra body {i:02}")))
            .collect();
        let standings: HashMap<_, _> = entries.iter().map(|e| (e.id, Standing::Ratified)).collect();
        let view = ChannelView {
            name: Some("t".into()),
            entries,
            party: Vec::new(),
            unrecognized: Default::default(),
            standings,
            gate_status: Default::default(),
            sessions: Default::default(),
            closed: false,
        };
        let brief = brief_markdown("t", &ChannelId::new(), &view);

        assert!(brief.contains("extra body 29"), "newest in full: {brief}");
        assert!(
            brief.contains("extra body 20"),
            "10th newest in full: {brief}"
        );
        assert!(
            brief.contains("decision 05 settled."),
            "clamped keeps claim"
        );
        assert!(
            !brief.contains("extra body 05"),
            "clamped drops body: {brief}"
        );
        assert!(
            !brief.contains("decision 04 settled"),
            "oldest omitted: {brief}"
        );
        assert!(brief.contains("…and 5 older standing decisions"), "{brief}");
    }

    #[test]
    fn html_escapes_user_content() {
        let view = view_with(vec![assertion("<script>alert('x')</script> & co")]);
        let html = channel_html(
            &[],
            "test",
            &ChannelId::new(),
            &view,
            std::path::Path::new("/repo"),
            None,
        );
        assert!(
            !html.contains("<script>alert"),
            "raw script must not appear"
        );
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("&amp; co"));
    }

    #[test]
    fn html_shows_standing_badge_and_timestamp() {
        let view = view_with(vec![assertion("claim")]);
        let html = channel_html(
            &[],
            "test",
            &ChannelId::new(),
            &view,
            std::path::Path::new("/repo"),
            None,
        );
        assert!(html.contains("badge provisional"));
        assert!(html.contains("2026-06-09"), "human-readable date: {html}");
    }

    #[test]
    fn html_shows_rationale_and_provenance() {
        // The record's *why* is content, not metadata — the page must carry it.
        let entry = LedgerEntry {
            id: EntryId::new(),
            channel: ChannelId::new(),
            author: Member::agent("Bot", "bot@example.com"),
            timestamp: Timestamp::from_millis(1_781_046_734_154),
            payload: EntryPayload::Assertion {
                statement: "claim".into(),
                rationale: "because the tests prove it".into(),
                provenance: vec![junto_kernel::ProvenanceRef::new(
                    junto_kernel::Uri::new("https://example.com/pr/1").expect("uri"),
                )],
                frame: None,
            },
        };
        let view = view_with(vec![entry]);
        let html = channel_html(
            &[],
            "test",
            &ChannelId::new(),
            &view,
            std::path::Path::new("/repo"),
            None,
        );
        assert!(html.contains("because the tests prove it"));
        assert!(html.contains("provenance (1)"));
        assert!(html.contains("https://example.com/pr/1"));
    }

    #[test]
    fn brief_lists_ids_for_targeting() {
        let entry = assertion("claim");
        let id = entry.id.to_string();
        let view = view_with(vec![entry]);
        let brief = brief_markdown("test", &ChannelId::new(), &view);
        assert!(brief.contains(&id));
        assert!(brief.contains("[provisional]"));
    }

    #[test]
    fn empty_channel_renders_in_both_styles() {
        let view = view_with(vec![]);
        let id = ChannelId::new();
        assert!(brief_markdown("empty", &id, &view).contains("(no entries)"));
        assert!(
            channel_html(
                &[],
                "empty",
                &id,
                &view,
                std::path::Path::new("/repo"),
                None
            )
            .contains("(no entries)")
        );
    }

    #[test]
    fn framed_entries_render_one_click_options() {
        // docs/adr/0019: a decision frame becomes one-click acts with the
        // drafted rationale editable in place; the free-text form remains.
        let entry = LedgerEntry {
            id: EntryId::new(),
            channel: ChannelId::new(),
            author: Member::agent("Bot", "bot@example.com"),
            timestamp: Timestamp::from_millis(1_781_046_734_154),
            payload: EntryPayload::Assertion {
                statement: "the fix holds".into(),
                rationale: "tests pass".into(),
                provenance: vec![],
                frame: Some(junto_kernel::DecisionFrame {
                    options: vec![
                        junto_kernel::FrameOption {
                            label: "verified".into(),
                            act: junto_kernel::FrameAct::Ratify,
                            rationale: "CI green and reviewed".into(),
                        },
                        junto_kernel::FrameOption {
                            label: "not convinced".into(),
                            act: junto_kernel::FrameAct::Park,
                            rationale: "evidence insufficient".into(),
                        },
                    ],
                }),
            },
        };
        let id = entry.id;
        let channel = entry.channel;
        let view = view_with(vec![entry]);
        let html = channel_html(
            &[],
            "test",
            &channel,
            &view,
            std::path::Path::new("/repo"),
            None,
        );
        assert!(html.contains(">verified</button>"), "{html}");
        assert!(html.contains("value=\"CI green and reviewed\""));
        assert!(html.contains(&format!("/channels/{channel}/entries/{id}/park")));
        assert!(
            html.contains("placeholder=\"why — a rationale, not a checkbox\""),
            "free-text form remains"
        );
    }

    #[test]
    fn sidebar_lists_channels_and_marks_the_active_one() {
        let active = ChannelId::new();
        let other = ChannelId::new();
        let nav = vec![
            ChannelSummary {
                id: active,
                name: Some("alpha".into()),
                substrate: std::path::PathBuf::from("/repo/a"),
                entry_count: 3,
                last_activity: None,
                open_gates: 2,
                members: 2,
                latest: Some("assertion — the latest finding".into()),
                closed: false,
            },
            ChannelSummary {
                id: other,
                name: Some("beta".into()),
                substrate: std::path::PathBuf::from("/repo/b"),
                entry_count: 1,
                last_activity: None,
                open_gates: 0,
                members: 1,
                latest: None,
                closed: false,
            },
        ];
        let view = view_with(vec![]);
        let html = channel_html(
            &nav,
            "alpha",
            &active,
            &view,
            std::path::Path::new("/repo"),
            None,
        );
        assert!(html.contains("chan active"));
        assert!(html.contains("alpha"));
        assert!(html.contains("beta"));
        assert!(
            html.contains("gatecount"),
            "open gates surface in the sidebar"
        );
    }
}
