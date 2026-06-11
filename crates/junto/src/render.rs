//! Rendering a channel projection for readers.
//!
//! Two audiences, one source of truth (the [`ChannelView`] projection):
//! - [`brief_markdown`] — the **agent** read path: the MCP `view_channel`
//!   tool and the `/channels/{name}/brief` endpoint the SessionStart recall
//!   hook injects into agent context (`docs/adr/0013`). Deliberately plain:
//!   ids, states, full text.
//! - [`index_html`] / [`channel_html`] — the **human** read path: the pages
//!   the desktop shell frames (`docs/adr/0018`). Server-rendered with shared
//!   app chrome (sidebar navigation, dark theme) and zero JS — `<details>`
//!   carries the expand/collapse. Its information design is product surface
//!   (`docs/adr/0013`), reviewed as such.

use junto_kernel::{
    ChannelId, ChannelView, EntryPayload, GateStatus, LedgerEntry, Member, MemberKind,
    ProvenanceRef, Standing,
};
use std::fmt::Write as _;

use crate::host::ChannelSummary;

/// `Name <email>` plus an `(agent)` marker — humans are the unmarked case.
fn member_label(member: &Member) -> String {
    let marker = match member.kind {
        MemberKind::Human => "",
        MemberKind::Agent => " (agent)",
    };
    format!("{} <{}>{marker}", member.display_name, member.email)
}

/// The agent-facing markdown brief: every entry in canonical order, with ids
/// (the targets for ratify/park/correct/approve/reject) and derived states.
pub fn brief_markdown(name: &str, id: &ChannelId, view: &ChannelView) -> String {
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
        EntryPayload::Assertion { statement, .. } => {
            format!(
                "**assertion** [{}] — {statement}",
                standing_label(view, entry)
            )
        }
        EntryPayload::Ratification { target, .. } => format!("ratification of `{target}`"),
        EntryPayload::Park { target, .. } => format!("park of `{target}`"),
        EntryPayload::Correction {
            target, statement, ..
        } => format!("correction of `{target}` — {statement}"),
        EntryPayload::Proposal { action, .. } => {
            format!("**proposal** [{}] — {action}", gate_label(view, entry))
        }
        EntryPayload::Approval { target, .. } => format!("approval of `{target}`"),
        EntryPayload::Rejection { target, .. } => format!("rejection of `{target}`"),
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

// ---- the human pages (the pixels the desktop shell frames) ----

/// The shared app chrome: wordmark + channel sidebar on the left, `content`
/// on the right. Every page is this shell with a different main pane.
fn page_shell(
    title: &str,
    nav: &[ChannelSummary],
    active: Option<&ChannelId>,
    content: &str,
) -> String {
    let mut links = String::new();
    for summary in nav {
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
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{title}</title>\n<style>{CSS}</style></head>\n\
         <body><div class=\"app\">\n\
         <nav class=\"side\">\n\
         <a class=\"brand\" href=\"/\"><span class=\"logo\">j</span>junto</a>\n\
         <div class=\"side-label\">channels</div>\n{links}\
         </nav>\n\
         <main>\n{content}</main>\n\
         </div></body></html>\n",
        title = escape_html(title),
    )
}

/// The channel index — every channel across every registered home substrate,
/// the landing page of the one surface (`docs/adr/0015`).
pub fn index_html(summaries: &[ChannelSummary]) -> String {
    let mut cards = String::new();
    for summary in summaries {
        let display_name = summary.name.as_deref().unwrap_or("(unopened)");
        let href = summary
            .name
            .clone()
            .unwrap_or_else(|| summary.id.to_string());
        let gates = if summary.open_gates > 0 {
            format!(
                "<span class=\"badge pending\">{} open gate{}</span>",
                summary.open_gates,
                if summary.open_gates == 1 { "" } else { "s" }
            )
        } else {
            "<span class=\"badge quiet\">no open gates</span>".to_string()
        };
        let when = summary
            .last_activity
            .map(|ts| iso_utc(ts.as_millis()))
            .unwrap_or_default();
        let _ = writeln!(
            cards,
            "<a class=\"card-link\" href=\"/channels/{href}\"><article class=\"card\">\
             <header><h2>{name}</h2>{gates}</header>\
             <div class=\"meta-line\">{count} entries · last activity {when}</div>\
             <footer class=\"id\">{id} · {substrate}</footer>\
             </article></a>",
            href = escape_html(&href),
            name = escape_html(display_name),
            count = summary.entry_count,
            when = escape_html(&when),
            id = summary.id,
            substrate = escape_html(&summary.substrate.display().to_string()),
        );
    }
    let body = if summaries.is_empty() {
        "<p class=\"empty\">no channels yet — open one with the open_channel tool or \
         `junto open`</p>"
            .to_string()
    } else {
        cards
    };
    let content = format!(
        "<h1>channels</h1>\n\
         <p class=\"meta\">{count} channel{plural} across every registered substrate · \
         read-only projection</p>\n{body}",
        count = summaries.len(),
        plural = if summaries.len() == 1 { "" } else { "s" },
    );
    page_shell("junto — channels", summaries, None, &content)
}

/// The human-facing channel page: the projected ledger as entry cards, with
/// rationale and provenance visible (collapsible) and verification forms
/// (ratify/park on provisional assertions, approve/reject on pending
/// proposals) — the human write surface. Forms post id-addressed URLs (ids
/// are URL-safe; names may not be) and require a rationale.
///
/// `nav` feeds the sidebar; pass `&[]` where navigation is irrelevant.
pub fn channel_html(
    nav: &[ChannelSummary],
    name: &str,
    id: &ChannelId,
    view: &ChannelView,
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
    let content = format!(
        "<h1>{name}</h1>\n\
         <p class=\"meta\">channel {id} · {count} entries · read-only projection</p>\n\
         {party}{body}",
        name = escape_html(name),
        count = view.entries.len(),
    );
    page_shell(&format!("junto — {name}"), nav, Some(id), &content)
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
        EntryPayload::Assertion {
            statement,
            rationale,
            provenance,
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
        "<article class=\"card{flag}\">\
         <header><span class=\"kind\">{kind}</span>{badge}{unrecognized_badge}\
         <span class=\"spacer\"></span>\
         <span class=\"who\" title=\"{email}\">{who}</span>\
         <span class=\"when\">{when}</span></header>\
         {statement}{target}{rationale}{provenance}\
         <footer class=\"id\">{id}</footer>\
         {form}</article>",
        flag = if unrecognized { " flagged" } else { "" },
        email = escape_html(&entry.author.email),
        who = escape_html(&entry.author.display_name),
        when = escape_html(&iso_utc(entry.timestamp.as_millis())),
        id = entry.id,
        form = verification_form(entry, view, channel),
    )
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
    format!(
        "<form class=\"act\" method=\"post\" \
         action=\"/channels/{channel}/entries/{entry_id}/{accept}\">\
         <input name=\"rationale\" placeholder=\"why — a rationale, not a checkbox\" required>\
         <input name=\"code\" class=\"code\" placeholder=\"member code\" \
         title=\"your machine-local member code (docs/adr/0017)\">\
         <button class=\"primary\">{accept}</button>\
         <button formaction=\"/channels/{channel}/entries/{entry_id}/{decline}\">{decline}</button>\
         </form>",
        entry_id = entry.id,
    )
}

/// The dark theme, keyed to the app icon palette (`docs/adr/0018`): one CSS
/// blob, no JS, no external assets — the pages must render identically in
/// the desktop shell's webview and a plain browser, offline.
const CSS: &str = "\
:root{--bg:#11111b;--panel:#181825;--card:#1e1e2e;--border:#313244;--text:#cdd6f4;\
--muted:#7f849c;--soft:#a6adc8;--accent:#89b4fa;--green:#a6e3a1;--yellow:#f9e2af;\
--red:#f38ba8;--gray:#9399b2}\
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
.chan-name{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.gatecount{flex:none;font-size:.7rem;font-weight:650;color:var(--bg);background:var(--yellow);\
border-radius:.6rem;padding:0 .45rem}\
main{flex:1;min-width:0;max-width:56rem;padding:2rem 2.25rem 4rem}\
h1{margin:0 0 .2rem;font-size:1.45rem}h2{margin:0;font-size:1.02rem}\
.meta{color:var(--muted);font-size:.82rem;margin:0 0 1.1rem}\
.empty{color:var(--muted)}\
.party{display:flex;flex-wrap:wrap;gap:.4rem;margin:0 0 1.2rem}\
.chip{font-size:.74rem;color:var(--soft);border:1px solid var(--border);\
border-radius:.7rem;padding:.12rem .6rem;background:var(--panel)}\
.card{background:var(--card);border:1px solid var(--border);border-radius:.65rem;\
padding:.8rem .95rem;margin-bottom:.7rem}\
.card.flagged{border-color:var(--red)}\
.card-link{text-decoration:none;color:inherit;display:block}\
.card-link:hover .card{border-color:var(--accent)}\
.card header{display:flex;align-items:center;gap:.55rem;flex-wrap:wrap}\
.spacer{flex:1}\
.kind{font-size:.72rem;text-transform:uppercase;letter-spacing:.06em;color:var(--accent);\
font-weight:650}\
.badge{font-size:.7rem;font-weight:600;padding:.08rem .55rem;border-radius:.6rem;\
text-transform:uppercase;letter-spacing:.04em;border:1px solid transparent}\
.provisional,.pending{color:var(--yellow);background:rgba(249,226,175,.12);\
border-color:rgba(249,226,175,.3)}\
.ratified,.approved{color:var(--green);background:rgba(166,227,161,.12);\
border-color:rgba(166,227,161,.3)}\
.parked,.superseded,.quiet{color:var(--gray);background:rgba(147,153,178,.12);\
border-color:rgba(147,153,178,.3)}\
.rejected,.unrecognized{color:var(--red);background:rgba(243,139,168,.12);\
border-color:rgba(243,139,168,.3)}\
.who{color:var(--soft);font-size:.82rem}\
.when{color:var(--muted);font-size:.76rem}\
.statement{margin:.55rem 0 0;white-space:pre-wrap}\
.meta-line{color:var(--muted);font-size:.8rem;margin-top:.45rem}\
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
font-family:ui-monospace,'Cascadia Mono',Consolas,monospace;margin-top:.55rem}\
form.act{display:flex;gap:.45rem;flex-wrap:wrap;margin-top:.65rem;padding-top:.65rem;\
border-top:1px solid var(--border)}\
form.act input{flex:1;min-width:10rem;background:var(--bg);color:var(--text);\
border:1px solid var(--border);border-radius:.45rem;padding:.32rem .6rem;font-size:.84rem}\
form.act input:focus{outline:1px solid var(--accent)}\
form.act input.code{flex:none;width:8.2rem}\
form.act button{background:var(--panel);color:var(--soft);border:1px solid var(--border);\
border-radius:.45rem;padding:.32rem .85rem;font-size:.84rem;cursor:pointer}\
form.act button:hover{color:var(--text);border-color:var(--accent)}\
form.act button.primary{background:rgba(137,180,250,.15);color:var(--accent);\
border-color:rgba(137,180,250,.4)}";

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
            },
        }
    }

    #[test]
    fn html_escapes_user_content() {
        let view = view_with(vec![assertion("<script>alert('x')</script> & co")]);
        let html = channel_html(&[], "test", &ChannelId::new(), &view);
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
        let html = channel_html(&[], "test", &ChannelId::new(), &view);
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
            },
        };
        let view = view_with(vec![entry]);
        let html = channel_html(&[], "test", &ChannelId::new(), &view);
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
        assert!(channel_html(&[], "empty", &id, &view).contains("(no entries)"));
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
            },
            ChannelSummary {
                id: other,
                name: Some("beta".into()),
                substrate: std::path::PathBuf::from("/repo/b"),
                entry_count: 1,
                last_activity: None,
                open_gates: 0,
            },
        ];
        let view = view_with(vec![]);
        let html = channel_html(&nav, "alpha", &active, &view);
        assert!(html.contains("chan active"));
        assert!(html.contains("alpha"));
        assert!(html.contains("beta"));
        assert!(
            html.contains("gatecount"),
            "open gates surface in the sidebar"
        );
    }
}
