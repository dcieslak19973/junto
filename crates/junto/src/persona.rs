//! Personas — named, reusable, machine-local configurations over a Harness
//! (`docs/superpowers/specs/2026-06-13-agent-personas-design.md`).
//!
//! A **Persona** is the thing a human picks when starting work: it references a
//! [`Harness`](crate::launch::Harness) (the engine) and carries a role, an
//! optional model, MCP servers, and (Claude-only) skills + local plugins.
//! One harness → many personas. The config is a **machine fact**
//! (`~/.junto/personas.toml`) and never enters the ledger; only the persona's
//! identity (its agent [`Member`]) lands there when it authors work.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use junto_kernel::Member;

use crate::launch::all_harnesses;

/// An MCP server a persona offers its agent — forwarded to the harness over ACP
/// (`session/new` `mcpServers`). v1 is URL-shaped (streamable HTTP); command /
/// env variants can join later (rule of three).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct McpServer {
    /// Unique name the agent sees for this server.
    pub(crate) name: String,
    /// The server's streamable-HTTP endpoint URL.
    pub(crate) url: String,
}

/// A named, reusable configuration over a harness — see the module docs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Persona {
    /// Stable id and member-email stem; immutable after creation.
    pub(crate) slug: String,
    /// Display label (and the agent member's name).
    pub(crate) name: String,
    /// The harness this persona runs on (`claude`, `opencode`).
    pub(crate) harness: String,
    /// The persona's stable agent-member identity (`<slug>@junto.local` for
    /// custom personas; the harness's own email for stock personas, so existing
    /// channels keep resolving).
    pub(crate) email: String,
    /// The role / system-prompt, if any.
    #[serde(default)]
    pub(crate) role: Option<String>,
    /// An optional model override.
    #[serde(default)]
    pub(crate) model: Option<String>,
    /// MCP servers the persona offers.
    #[serde(default)]
    pub(crate) mcp_servers: Vec<McpServer>,
    /// Claude-only: skills to enable, by name (matching the `SKILL.md` `name`
    /// or `plugin:skill`). Delivered as the SDK `skills` option; only enables
    /// skills already discovered on the machine.
    #[serde(default)]
    pub(crate) skills: Vec<String>,
    /// Claude-only: local plugin directories to load (absolute paths).
    /// Delivered as SDK `plugins: [{type:"local", path}]`. (Remote plugin
    /// marketplaces aren't supported by the SDK option yet — out of scope.)
    #[serde(default)]
    pub(crate) plugins: Vec<String>,
}

impl Persona {
    /// The persona's agent-member identity — sessions and outcomes are authored
    /// as the persona, never the operator (`docs/adr/0012`/`0020`).
    pub(crate) fn member(&self) -> Member {
        Member::agent(self.name.clone(), self.email.clone())
    }
}

/// The on-disk shape of `~/.junto/personas.toml`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct PersonasFile {
    #[serde(default)]
    personas: Vec<Persona>,
}

fn personas_path(junto_home: &Path) -> PathBuf {
    junto_home.join("personas.toml")
}

/// One stock persona per registered harness — the bare engine with no extra
/// config, identified by the harness's own member email. Used to seed an empty
/// store so the launch picker is never empty and there's something to clone.
fn stock_personas() -> Vec<Persona> {
    all_harnesses()
        .iter()
        .map(|harness| Persona {
            slug: harness.id.to_string(),
            name: harness.label.to_string(),
            harness: harness.id.to_string(),
            email: harness.email.to_string(),
            role: None,
            model: None,
            mcp_servers: Vec::new(),
            skills: Vec::new(),
            plugins: Vec::new(),
        })
        .collect()
}

/// A skill discovered on this machine, for the persona form's picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiscoveredSkill {
    /// The skill's name (matches the SDK `skills` option and `SKILL.md` `name`).
    pub(crate) name: String,
    /// The one-line description from `SKILL.md` frontmatter, for the picker.
    pub(crate) description: String,
}

/// The Claude config dir skills are discovered from — `CLAUDE_CONFIG_DIR` if
/// set (matching the ACP adapter), else `~/.claude`. The persona's `skills`
/// option enables among the skills in this dir's `skills/` subtree.
fn claude_config_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    dirs_home().map(|home| home.join(".claude"))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Skills installed under the Claude config dir's `skills/`, each parsed for
/// its `name` + `description` from `SKILL.md` frontmatter. Empty when the dir
/// is absent — discovery is best-effort (the picker just shows nothing).
pub(crate) fn discover_skills() -> Vec<DiscoveredSkill> {
    let Some(skills_dir) = claude_config_dir().map(|dir| dir.join("skills")) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&skills_dir) else {
        return Vec::new();
    };
    let mut skills: Vec<DiscoveredSkill> = entries
        .flatten()
        .filter_map(|entry| {
            let text = std::fs::read_to_string(entry.path().join("SKILL.md")).ok()?;
            let dir_name = entry.file_name().to_string_lossy().into_owned();
            Some(parse_skill_frontmatter(&text, &dir_name))
        })
        .collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Pull `name` and `description` from a `SKILL.md` YAML frontmatter block,
/// falling back to the directory name. Handles inline values and folded/literal
/// block scalars (`description: >` / `|`) by gathering the indented lines that
/// follow. Deliberately tiny — a full YAML parse isn't warranted for two keys.
fn parse_skill_frontmatter(text: &str, dir_name: &str) -> DiscoveredSkill {
    let mut name = dir_name.to_string();
    let mut description = String::new();
    let mut lines = text.lines();
    // The frontmatter is the block between the first two `---` fences.
    if lines.next().map(str::trim) != Some("---") {
        return DiscoveredSkill { name, description };
    }
    let mut pending: Vec<String> = Vec::new();
    while let Some(line) = lines.next() {
        if line.trim() == "---" {
            break;
        }
        if let Some(rest) = line.strip_prefix("name:") {
            name = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("description:") {
            let rest = rest.trim();
            if rest.is_empty() || rest == ">" || rest == "|" || rest == ">-" || rest == "|-" {
                // A block scalar: gather the indented lines that follow, until
                // a non-indented line (the next key, or the closing `---`).
                pending = lines
                    .by_ref()
                    .take_while(|l| l.starts_with(char::is_whitespace))
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect();
                break;
            }
            description = rest.to_string();
        }
    }
    if description.is_empty() {
        description = pending.join(" ");
    }
    DiscoveredSkill { name, description }
}

/// Every persona — the stored set, or the stock seed when none are stored yet.
/// Seed-on-read (computed, not written): the file stays empty until the user
/// actually saves an edit.
pub(crate) fn all_personas(junto_home: &Path) -> Result<Vec<Persona>> {
    let stored = read_file(junto_home)?.personas;
    if stored.is_empty() {
        Ok(stock_personas())
    } else {
        Ok(stored)
    }
}

/// The persona already serving a channel — the persona whose agent member is in
/// `party`, if any. This resolves the established agent at the persona layer:
/// one agent per channel
/// (`docs/adr/0024`/`docs/superpowers/specs/2026-06-13-agent-personas-design.md`),
/// so a launch reuses the established persona and the picker only appears before
/// one is set. The persona's harness drives the turn.
pub(crate) fn channel_persona(junto_home: &Path, party: &[Member]) -> Result<Option<Persona>> {
    Ok(all_personas(junto_home)?
        .into_iter()
        .find(|persona| party.iter().any(|member| member.email == persona.email)))
}

/// The persona for a slug, resolving against the stock seed for an empty store.
pub(crate) fn persona_by_slug(junto_home: &Path, slug: &str) -> Result<Option<Persona>> {
    Ok(all_personas(junto_home)?
        .into_iter()
        .find(|persona| persona.slug == slug))
}

/// Save (insert or replace by slug) a persona. The first save of any persona
/// materializes the store, so the stock seed must be folded in first — without
/// it, saving one edited stock persona would silently drop the others.
pub(crate) fn save_persona(junto_home: &Path, persona: Persona) -> Result<()> {
    let mut personas = all_personas(junto_home)?;
    match personas.iter_mut().find(|p| p.slug == persona.slug) {
        Some(existing) => *existing = persona,
        None => personas.push(persona),
    }
    write_file(junto_home, &PersonasFile { personas })
}

/// Delete a persona by slug. Deleting from an unmaterialized store first folds
/// in the stock seed, so removing one stock persona keeps the rest.
pub(crate) fn delete_persona(junto_home: &Path, slug: &str) -> Result<()> {
    let mut personas = all_personas(junto_home)?;
    personas.retain(|p| p.slug != slug);
    write_file(junto_home, &PersonasFile { personas })
}

fn read_file(junto_home: &Path) -> Result<PersonasFile> {
    let path = personas_path(junto_home);
    if !path.exists() {
        return Ok(PersonasFile::default());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

fn write_file(junto_home: &Path, file: &PersonasFile) -> Result<()> {
    std::fs::create_dir_all(junto_home)
        .with_context(|| format!("creating {}", junto_home.display()))?;
    let path = personas_path(junto_home);
    std::fs::write(
        &path,
        toml::to_string_pretty(file).context("serializing personas")?,
    )
    .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(slug: &str) -> Persona {
        Persona {
            slug: slug.to_string(),
            name: "Security Reviewer".to_string(),
            harness: "claude".to_string(),
            email: format!("{slug}@junto.local"),
            role: Some("be thorough".to_string()),
            model: Some("claude-opus-4-8".to_string()),
            mcp_servers: vec![McpServer {
                name: "junto".to_string(),
                url: "http://127.0.0.1:1727/mcp".to_string(),
            }],
            skills: vec!["security-review".to_string()],
            plugins: vec![],
        }
    }

    #[test]
    fn parse_skill_frontmatter_reads_inline_and_folded_descriptions() {
        let folded = "---\nname: caveman\ndescription: >\n  Ultra-compressed mode.\n  Cuts tokens.\n---\nbody";
        let skill = parse_skill_frontmatter(folded, "caveman");
        assert_eq!(skill.name, "caveman");
        assert_eq!(skill.description, "Ultra-compressed mode. Cuts tokens.");

        let inline = "---\nname: diagnose\ndescription: A debugging loop.\n---\n";
        let skill = parse_skill_frontmatter(inline, "diagnose");
        assert_eq!(skill.name, "diagnose");
        assert_eq!(skill.description, "A debugging loop.");

        // No frontmatter → fall back to the directory name, empty description.
        let none = parse_skill_frontmatter("# just a heading\n", "my-skill");
        assert_eq!(none.name, "my-skill");
        assert_eq!(none.description, "");
    }

    #[test]
    fn empty_store_seeds_stock_personas() {
        let home = tempfile::tempdir().expect("tempdir");
        let personas = all_personas(home.path()).expect("all_personas");
        // One per registered harness, identified by the harness's own email.
        assert_eq!(personas.len(), all_harnesses().len());
        assert!(
            personas
                .iter()
                .any(|p| p.slug == "claude" && p.email == "claude-code@anthropic.com"),
            "stock Claude persona reuses the harness email so existing channels resolve"
        );
        // Seed-on-read does not write the file.
        assert!(!personas_path(home.path()).exists());
    }

    #[test]
    fn save_then_load_round_trips() {
        let home = tempfile::tempdir().expect("tempdir");
        save_persona(home.path(), sample("security-reviewer")).expect("save");
        let found = persona_by_slug(home.path(), "security-reviewer")
            .expect("by_slug")
            .expect("present");
        assert_eq!(found, sample("security-reviewer"));
    }

    #[test]
    fn first_save_materializes_store_with_stock_preserved() {
        let home = tempfile::tempdir().expect("tempdir");
        save_persona(home.path(), sample("security-reviewer")).expect("save");
        let personas = all_personas(home.path()).expect("all_personas");
        // The stock seed survives the first custom save.
        assert!(personas.iter().any(|p| p.slug == "claude"));
        assert!(personas.iter().any(|p| p.slug == "security-reviewer"));
    }

    #[test]
    fn save_replaces_by_slug() {
        let home = tempfile::tempdir().expect("tempdir");
        save_persona(home.path(), sample("reviewer")).expect("save");
        let mut edited = sample("reviewer");
        edited.name = "Renamed".to_string();
        save_persona(home.path(), edited).expect("re-save");
        let all = all_personas(home.path()).expect("all");
        assert_eq!(all.iter().filter(|p| p.slug == "reviewer").count(), 1);
        assert_eq!(
            persona_by_slug(home.path(), "reviewer")
                .expect("by_slug")
                .expect("present")
                .name,
            "Renamed"
        );
    }

    #[test]
    fn delete_removes_by_slug() {
        let home = tempfile::tempdir().expect("tempdir");
        save_persona(home.path(), sample("reviewer")).expect("save");
        delete_persona(home.path(), "reviewer").expect("delete");
        assert!(
            persona_by_slug(home.path(), "reviewer")
                .expect("by_slug")
                .is_none()
        );
    }

    #[test]
    fn channel_persona_resolves_the_established_persona() {
        let home = tempfile::tempdir().expect("tempdir");
        save_persona(home.path(), sample("security-reviewer")).expect("save");
        let party = vec![
            Member::human("Dan", "dan@example.com"),
            Member::agent("Security Reviewer", "security-reviewer@junto.local"),
        ];
        let resolved = channel_persona(home.path(), &party)
            .expect("channel_persona")
            .expect("present");
        assert_eq!(resolved.slug, "security-reviewer");
        assert_eq!(resolved.harness, "claude");
    }

    #[test]
    fn channel_persona_is_none_without_a_persona_member() {
        let home = tempfile::tempdir().expect("tempdir");
        let party = vec![Member::human("Dan", "dan@example.com")];
        assert!(
            channel_persona(home.path(), &party)
                .expect("channel_persona")
                .is_none()
        );
    }

    #[test]
    fn persona_authors_as_itself() {
        let member = sample("security-reviewer").member();
        assert_eq!(member.email, "security-reviewer@junto.local");
        assert_eq!(member.display_name, "Security Reviewer");
    }
}
