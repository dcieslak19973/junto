//! Agents — named, reusable, machine-local configurations over a Harness
//! (`docs/superpowers/specs/2026-06-13-agent-personas-design.md`).
//!
//! An **Agent** is the thing a human picks when starting work: it references a
//! [`Harness`](crate::launch::Harness) (the engine) and carries a role, an
//! optional model, MCP servers, and (Claude-only) skills + local plugins.
//! One harness → many Agents. The config is a **machine fact**
//! (`~/.junto/agents.toml`) and never enters the ledger; only the Agent's
//! identity (its [`Member`]) lands there when it authors work.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use junto_kernel::Member;

use crate::launch::all_harnesses;

/// An MCP server an Agent offers — forwarded to the harness over ACP
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
pub(crate) struct Agent {
    /// Stable id and member-email stem; immutable after creation.
    pub(crate) slug: String,
    /// Display label (and the agent member's name).
    pub(crate) name: String,
    /// The harness this agent runs on (`claude`, `opencode`).
    pub(crate) harness: String,
    /// The agent's stable agent-member identity (`<slug>@junto.local` for
    /// custom agents; the harness's own email for stock agents, so existing
    /// channels keep resolving).
    pub(crate) email: String,
    /// The role / system-prompt, if any.
    #[serde(default)]
    pub(crate) role: Option<String>,
    /// An optional model override.
    #[serde(default)]
    pub(crate) model: Option<String>,
    /// MCP servers the agent offers.
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

impl Agent {
    /// The Agent's member identity — sessions and deliverables are authored
    /// as the agent, never the operator (`docs/adr/0012`/`0020`).
    pub(crate) fn member(&self) -> Member {
        Member::agent(self.name.clone(), self.email.clone())
    }
}

/// The on-disk shape of `~/.junto/agents.toml`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct AgentsFile {
    #[serde(default)]
    agents: Vec<Agent>,
}

fn agents_path(junto_home: &Path) -> PathBuf {
    junto_home.join("agents.toml")
}

/// One stock agent per registered harness — the bare engine with no extra
/// config, identified by the harness's own member email. Used to seed an empty
/// store so the launch picker is never empty and there's something to clone.
fn stock_agents() -> Vec<Agent> {
    all_harnesses()
        .iter()
        .map(|harness| Agent {
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

/// A skill discovered on this machine, for the agent form's picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiscoveredSkill {
    /// The skill's name (matches the SDK `skills` option and `SKILL.md` `name`).
    pub(crate) name: String,
    /// The one-line description from `SKILL.md` frontmatter, for the picker.
    pub(crate) description: String,
}

/// The Claude config dir skills are discovered from — `CLAUDE_CONFIG_DIR` if
/// set (matching the ACP adapter), else `~/.claude`. The agent's `skills`
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

/// Every agent — the stored set, or the stock seed when none are stored yet.
/// Seed-on-read (computed, not written): the file stays empty until the user
/// actually saves an edit.
pub(crate) fn all_agents(junto_home: &Path) -> Result<Vec<Agent>> {
    let stored = read_file(junto_home)?.agents;
    if stored.is_empty() {
        Ok(stock_agents())
    } else {
        Ok(stored)
    }
}

/// The agent already serving a channel — the agent whose agent member is in
/// `party`, if any. This resolves the established agent at the agent layer:
/// one agent per channel
/// (`docs/adr/0024`/`docs/superpowers/specs/2026-06-13-agent-personas-design.md`),
/// so a launch reuses the established agent and the picker only appears before
/// one is set. The agent's harness drives the turn.
pub(crate) fn channel_agent(junto_home: &Path, party: &[Member]) -> Result<Option<Agent>> {
    Ok(all_agents(junto_home)?
        .into_iter()
        .find(|agent| party.iter().any(|member| member.email == agent.email)))
}

/// The agent for a slug, resolving against the stock seed for an empty store.
pub(crate) fn agent_by_slug(junto_home: &Path, slug: &str) -> Result<Option<Agent>> {
    Ok(all_agents(junto_home)?
        .into_iter()
        .find(|agent| agent.slug == slug))
}

/// Save (insert or replace by slug) an Agent. The first save of any Agent
/// materializes the store, so the stock seed must be folded in first — without
/// it, saving one edited stock agent would silently drop the others.
pub(crate) fn save_agent(junto_home: &Path, agent: Agent) -> Result<()> {
    let mut agents = all_agents(junto_home)?;
    match agents.iter_mut().find(|p| p.slug == agent.slug) {
        Some(existing) => *existing = agent,
        None => agents.push(agent),
    }
    write_file(junto_home, &AgentsFile { agents })
}

/// Delete an Agent by slug. Deleting from an unmaterialized store first folds
/// in the stock seed, so removing one stock agent keeps the rest.
pub(crate) fn delete_agent(junto_home: &Path, slug: &str) -> Result<()> {
    let mut agents = all_agents(junto_home)?;
    agents.retain(|p| p.slug != slug);
    write_file(junto_home, &AgentsFile { agents })
}

fn read_file(junto_home: &Path) -> Result<AgentsFile> {
    let path = agents_path(junto_home);
    if !path.exists() {
        return Ok(AgentsFile::default());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

fn write_file(junto_home: &Path, file: &AgentsFile) -> Result<()> {
    std::fs::create_dir_all(junto_home)
        .with_context(|| format!("creating {}", junto_home.display()))?;
    let path = agents_path(junto_home);
    std::fs::write(
        &path,
        toml::to_string_pretty(file).context("serializing agents")?,
    )
    .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(slug: &str) -> Agent {
        Agent {
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
    fn empty_store_seeds_stock_agents() {
        let home = tempfile::tempdir().expect("tempdir");
        let agents = all_agents(home.path()).expect("all_agents");
        // One per registered harness, identified by the harness's own email.
        assert_eq!(agents.len(), all_harnesses().len());
        assert!(
            agents
                .iter()
                .any(|p| p.slug == "claude" && p.email == "claude-code@anthropic.com"),
            "stock Claude agent reuses the harness email so existing channels resolve"
        );
        // Seed-on-read does not write the file.
        assert!(!agents_path(home.path()).exists());
    }

    #[test]
    fn save_then_load_round_trips() {
        let home = tempfile::tempdir().expect("tempdir");
        save_agent(home.path(), sample("security-reviewer")).expect("save");
        let found = agent_by_slug(home.path(), "security-reviewer")
            .expect("by_slug")
            .expect("present");
        assert_eq!(found, sample("security-reviewer"));
    }

    #[test]
    fn first_save_materializes_store_with_stock_preserved() {
        let home = tempfile::tempdir().expect("tempdir");
        save_agent(home.path(), sample("security-reviewer")).expect("save");
        let agents = all_agents(home.path()).expect("all_agents");
        // The stock seed survives the first custom save.
        assert!(agents.iter().any(|p| p.slug == "claude"));
        assert!(agents.iter().any(|p| p.slug == "security-reviewer"));
    }

    #[test]
    fn save_replaces_by_slug() {
        let home = tempfile::tempdir().expect("tempdir");
        save_agent(home.path(), sample("reviewer")).expect("save");
        let mut edited = sample("reviewer");
        edited.name = "Renamed".to_string();
        save_agent(home.path(), edited).expect("re-save");
        let all = all_agents(home.path()).expect("all");
        assert_eq!(all.iter().filter(|p| p.slug == "reviewer").count(), 1);
        assert_eq!(
            agent_by_slug(home.path(), "reviewer")
                .expect("by_slug")
                .expect("present")
                .name,
            "Renamed"
        );
    }

    #[test]
    fn delete_removes_by_slug() {
        let home = tempfile::tempdir().expect("tempdir");
        save_agent(home.path(), sample("reviewer")).expect("save");
        delete_agent(home.path(), "reviewer").expect("delete");
        assert!(
            agent_by_slug(home.path(), "reviewer")
                .expect("by_slug")
                .is_none()
        );
    }

    #[test]
    fn channel_agent_resolves_the_established_agent() {
        let home = tempfile::tempdir().expect("tempdir");
        save_agent(home.path(), sample("security-reviewer")).expect("save");
        let party = vec![
            Member::human("Dan", "dan@example.com"),
            Member::agent("Security Reviewer", "security-reviewer@junto.local"),
        ];
        let resolved = channel_agent(home.path(), &party)
            .expect("channel_agent")
            .expect("present");
        assert_eq!(resolved.slug, "security-reviewer");
        assert_eq!(resolved.harness, "claude");
    }

    #[test]
    fn channel_agent_is_none_without_a_agent_member() {
        let home = tempfile::tempdir().expect("tempdir");
        let party = vec![Member::human("Dan", "dan@example.com")];
        assert!(
            channel_agent(home.path(), &party)
                .expect("channel_agent")
                .is_none()
        );
    }

    #[test]
    fn agent_authors_as_itself() {
        let member = sample("security-reviewer").member();
        assert_eq!(member.email, "security-reviewer@junto.local");
        assert_eq!(member.display_name, "Security Reviewer");
    }
}
