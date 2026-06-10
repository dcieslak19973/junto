//! Rendering a channel projection for readers.
//!
//! Two audiences, one source of truth (the [`ChannelView`] projection):
//! - [`brief_markdown`] — the **agent** read path: the MCP `view_channel`
//!   tool and the `/channels/{name}/brief` endpoint the SessionStart recall
//!   hook injects into agent context (`docs/adr/0013`).
//! - [`channel_html`] — the **human** read path: the first pixel of junto's
//!   one surface, a read-only projection page served by the host.

use junto_kernel::{ChannelId, ChannelView, EntryPayload, GateStatus, LedgerEntry, Standing};
use std::fmt::Write as _;

/// The agent-facing markdown brief: every entry in canonical order, with ids
/// (the targets for ratify/park/correct/approve/reject) and derived states.
pub fn brief_markdown(name: &str, id: &ChannelId, view: &ChannelView) -> String {
    let mut out = format!("# channel '{name}' ({id})\n\n");
    if view.entries.is_empty() {
        out.push_str("(no entries)\n");
        return out;
    }
    let _ = writeln!(out, "{} entries, canonical order:\n", view.entries.len());
    for entry in &view.entries {
        let when = entry.timestamp.as_millis();
        let who = format!("{} <{}>", entry.author.display_name, entry.author.email);
        let _ = writeln!(
            out,
            "- `{}` @{when} {who}: {}",
            entry.id,
            describe(entry, view, MarkdownStyle)
        );
    }
    out
}

/// The channel index — every channel across every registered home substrate,
/// the landing page of the one surface (`docs/adr/0015`).
pub fn index_html(summaries: &[crate::host::ChannelSummary]) -> String {
    let mut rows = String::new();
    for summary in summaries {
        let display_name = summary.name.as_deref().unwrap_or("(unopened)");
        let link_target = summary
            .name
            .clone()
            .unwrap_or_else(|| summary.id.to_string());
        let gates = if summary.open_gates > 0 {
            format!(
                " <span class=\"badge pending\">{} open gate{}</span>",
                summary.open_gates,
                if summary.open_gates == 1 { "" } else { "s" }
            )
        } else {
            String::new()
        };
        let when = summary
            .last_activity
            .map(|ts| iso_utc(ts.as_millis()))
            .unwrap_or_default();
        let _ = writeln!(
            rows,
            "<li class=\"entry\"><a href=\"/channels/{href}\">{name}</a>{gates} \
             <span class=\"who\">{count} entries</span> \
             <span class=\"when\">{when}</span>\
             <div class=\"id\">{id} · {substrate}</div></li>",
            href = escape_html(&link_target),
            name = escape_html(display_name),
            count = summary.entry_count,
            when = escape_html(&when),
            id = summary.id,
            substrate = escape_html(&summary.substrate.display().to_string()),
        );
    }
    let body = if summaries.is_empty() {
        "<p>(no channels yet — open one with the open_channel tool or `junto open`)</p>".to_string()
    } else {
        format!("<ol class=\"ledger\">\n{rows}</ol>")
    };
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>junto — channels</title>\n<style>{CSS}</style></head>\n\
         <body><main>\n<h1>channels</h1>\n\
         <p class=\"meta\">{count} channel{plural} across every registered substrate · \
         read-only projection</p>\n\
         {body}\n</main></body></html>\n",
        count = summaries.len(),
        plural = if summaries.len() == 1 { "" } else { "s" },
    )
}

/// The human-facing channel page: the projected ledger, with verification
/// forms (ratify/park on provisional assertions, approve/reject on pending
/// proposals) — the human write surface. Forms post id-addressed URLs (ids
/// are URL-safe; names may not be) and require a rationale.
pub fn channel_html(name: &str, id: &ChannelId, view: &ChannelView) -> String {
    let mut rows = String::new();
    for entry in &view.entries {
        let _ = writeln!(
            rows,
            "<li class=\"entry\"><span class=\"when\">{}</span> \
             <span class=\"who\">{}</span> {}\
             <div class=\"id\">{}</div>{}</li>",
            escape_html(&iso_utc(entry.timestamp.as_millis())),
            escape_html(&format!(
                "{} <{}>",
                entry.author.display_name, entry.author.email
            )),
            describe(entry, view, HtmlStyle),
            entry.id,
            verification_form(entry, view, id),
        );
    }
    let body = if view.entries.is_empty() {
        "<p>(no entries)</p>".to_string()
    } else {
        format!("<ol class=\"ledger\">\n{rows}</ol>")
    };
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>junto — {name}</title>\n<style>{CSS}</style></head>\n\
         <body><main>\n<h1>{name}</h1>\n\
         <p class=\"meta\">channel {id} · {count} entries · read-only projection</p>\n\
         {body}\n</main></body></html>\n",
        name = escape_html(name),
        count = view.entries.len(),
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
         <button>{accept}</button>\
         <button formaction=\"/channels/{channel}/entries/{entry_id}/{decline}\">{decline}</button>\
         </form>",
        entry_id = entry.id,
    )
}

const CSS: &str = "body{font-family:system-ui,sans-serif;margin:2rem auto;max-width:48rem;\
padding:0 1rem;color:#1a1a1a}h1{margin-bottom:.25rem}.meta{color:#666;font-size:.85rem}\
ol.ledger{list-style:none;padding:0}li.entry{padding:.6rem .2rem;border-bottom:1px solid #eee}\
.when{color:#666;font-size:.8rem;margin-right:.5rem}.who{color:#444;font-size:.85rem}\
.id{color:#aaa;font-size:.7rem;font-family:monospace}.statement{margin:.2rem 0}\
.badge{font-size:.7rem;padding:.1rem .45rem;border-radius:.6rem;margin-left:.4rem;\
text-transform:uppercase;letter-spacing:.03em}.provisional{background:#fff3cd}\
.ratified{background:#d4edda}.parked{background:#e2e3e5}.superseded{background:#e2e3e5}\
.pending{background:#fff3cd}.approved{background:#d4edda}.rejected{background:#f8d7da}\
.kind{color:#888;font-size:.75rem;text-transform:uppercase;letter-spacing:.03em}\
form.act{margin:.45rem 0 0}form.act input{font-size:.8rem;padding:.2rem .45rem;\
width:22rem;max-width:60%}form.act button{font-size:.8rem;margin-left:.35rem;\
padding:.2rem .7rem;cursor:pointer}";

/// How [`describe`] should dress an entry: markdown backticks vs HTML spans.
trait Style {
    fn emphasis(&self, kind: &str) -> String;
    fn badge(&self, label: &str) -> String;
    fn target(&self, target: &str) -> String;
    fn text(&self, text: &str) -> String;
}

struct MarkdownStyle;
impl Style for MarkdownStyle {
    fn emphasis(&self, kind: &str) -> String {
        format!("**{kind}**")
    }
    fn badge(&self, label: &str) -> String {
        format!("[{label}]")
    }
    fn target(&self, target: &str) -> String {
        format!("`{target}`")
    }
    fn text(&self, text: &str) -> String {
        text.to_string()
    }
}

struct HtmlStyle;
impl Style for HtmlStyle {
    fn emphasis(&self, kind: &str) -> String {
        format!("<span class=\"kind\">{kind}</span>")
    }
    fn badge(&self, label: &str) -> String {
        format!("<span class=\"badge {label}\">{label}</span>")
    }
    fn target(&self, target: &str) -> String {
        format!("<code>{}</code>", escape_html(target))
    }
    fn text(&self, text: &str) -> String {
        format!("<div class=\"statement\">{}</div>", escape_html(text))
    }
}

/// One entry described in the given style, with its derived state attached.
fn describe(entry: &LedgerEntry, view: &ChannelView, style: impl Style) -> String {
    match &entry.payload {
        EntryPayload::ChannelOpened { name } => {
            format!(
                "{} — channel '{}' opened",
                style.emphasis("genesis"),
                style.text(name)
            )
        }
        EntryPayload::Assertion { statement, .. } => {
            let standing = match view.standing(&entry.id) {
                Some(Standing::Provisional) => "provisional",
                Some(Standing::Ratified) => "ratified",
                Some(Standing::Parked) => "parked",
                Some(Standing::Superseded) => "superseded",
                None => "unknown",
            };
            format!(
                "{} {} — {}",
                style.emphasis("assertion"),
                style.badge(standing),
                style.text(statement)
            )
        }
        EntryPayload::Ratification { target, .. } => {
            format!("ratification of {}", style.target(&target.to_string()))
        }
        EntryPayload::Park { target, .. } => {
            format!("park of {}", style.target(&target.to_string()))
        }
        EntryPayload::Correction {
            target, statement, ..
        } => format!(
            "correction of {} — {}",
            style.target(&target.to_string()),
            style.text(statement)
        ),
        EntryPayload::Proposal { action, .. } => {
            let status = match view.gate_status(&entry.id) {
                Some(GateStatus::Pending) => "pending",
                Some(GateStatus::Approved) => "approved",
                Some(GateStatus::Rejected) => "rejected",
                None => "unknown",
            };
            format!(
                "{} {} — {}",
                style.emphasis("proposal"),
                style.badge(status),
                style.text(action)
            )
        }
        EntryPayload::Approval { target, .. } => {
            format!("approval of {}", style.target(&target.to_string()))
        }
        EntryPayload::Rejection { target, .. } => {
            format!("rejection of {}", style.target(&target.to_string()))
        }
    }
}

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
        let html = channel_html("test", &ChannelId::new(), &view);
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
        let html = channel_html("test", &ChannelId::new(), &view);
        assert!(html.contains("badge provisional"));
        assert!(html.contains("2026-06-09"), "human-readable date: {html}");
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
        assert!(channel_html("empty", &id, &view).contains("(no entries)"));
    }
}
