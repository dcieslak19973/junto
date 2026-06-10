//! `junto init` — plumbing + binding for one project repo (`docs/adr/0015`).
//!
//! Init is *setup*, not a channel act: it (1) registers the repo as a home
//! substrate in the machine registry, (2) wires the agent harness — the
//! `.mcp.json` server entry and the SessionStart recall hook (`junto brief`),
//! (3) writes the committed channel binding, (4) writes the **consult/record
//! convention** — a junto-owned `.junto.convention.md` plus one stable
//! `@`-import line in the repo's CLAUDE.md, so agents treat the convention as
//! standing orders and init re-runs can refresh the text without touching the
//! user's prose — (5) optionally **opens** the ambient channel (the one
//! recorded act here, `docs/adr/0014`/`0016`), writing the genesis directly
//! into the substrate — no running host needed — and (6) optionally grants an
//! agent membership and writes its code relay (`--agent-name`/`--agent-email`,
//! `docs/adr/0017`), taking a bare repo to a working wedge in one command.
//!
//! Every step is idempotent and merge-preserving: existing `.mcp.json` /
//! `.claude/settings.json` / CLAUDE.md content is kept, junto's entries are
//! added beside it; only the junto-owned convention file is rewritten.

use std::path::Path;

use anyhow::{Context, Result, bail};

use junto_kernel::Member;

use crate::{binding, host};

/// The localhost MCP endpoint every project points at — one host per
/// machine/user (docs/adr/0015), so this is the same URL everywhere.
const MCP_URL: &str = "http://127.0.0.1:1727/mcp";
/// The SessionStart hook command: a single cross-platform executable
/// invocation (no shell operators — see CLAUDE.md hooks rule) that prints the
/// briefs of every channel this checkout is bound to.
const BRIEF_COMMAND: &str = "junto brief";

/// Run init for `repo`. `channel` names the ambient channel for the committed
/// binding (defaults to the repo's directory name); `open` also opens it;
/// `agent` grants that member agent membership and writes its code relay into
/// this checkout (the one-command wedge).
pub async fn run(
    repo: &Path,
    channel: Option<String>,
    open: bool,
    agent: Option<Member>,
) -> Result<()> {
    let repo =
        dunce::canonicalize(repo).with_context(|| format!("repo {} not found", repo.display()))?;
    if !repo.join(".git").exists() {
        bail!(
            "{} is not a git repository (the home substrate stores the record in refs/junto/*)",
            repo.display()
        );
    }

    let channel = match channel {
        Some(name) => name,
        None => repo
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .context("repo path has no directory name; pass --channel")?,
    };

    let junto_home = host::junto_home()?;
    host::register_substrate(&junto_home, &repo)?;
    println!(
        "registered {} as a home substrate ({})",
        repo.display(),
        junto_home.join("substrates.toml").display()
    );

    wire_mcp_json(&repo)?;
    wire_session_start_hook(&repo)?;
    gitignore_local_binding(&repo)?;

    binding::write_project_binding(&repo, std::slice::from_ref(&channel))?;
    println!(
        "bound this project to channel '{channel}' ({})",
        binding::PROJECT_BINDING
    );

    write_convention(&repo, &channel)?;
    wire_claude_md_import(&repo)?;

    let host = host::Host::fixed(vec![repo.clone()]);
    if open {
        let opened_by = host::git_user(&repo)?;
        let opened = host
            .open_channel(Some(&repo), &channel, opened_by, None)
            .await?;
        println!("opened channel '{channel}' (id {})", opened.id);
        print_founder_code(&opened);
    } else {
        println!("(channel '{channel}' not opened — run `junto open \"{channel}\"` when ready)");
    }

    if let Some(agent) = agent {
        let granted_by = host::git_user(&repo)?;
        let email = agent.email.clone();
        let minted = host.add_member(&channel, &granted_by, agent).await?;
        binding::write_local_member_code(&repo, &minted.code)?;
        println!(
            "granted {email} membership in '{channel}' and wrote its code relay \
             ({} — gitignored; the session brief carries it)",
            binding::LOCAL_BINDING
        );
    }

    println!(
        "done. start the host with `junto serve`; agents connect via .mcp.json, \
         sessions get briefs via the SessionStart hook (`junto brief` must be on PATH)."
    );
    Ok(())
}

/// The junto-owned convention file's name, relative to the repo root.
pub const CONVENTION_FILE: &str = ".junto.convention.md";

/// Write (or rewrite) the junto-owned convention file — the consult/record
/// standing orders an agent reads via the CLAUDE.md `@`-import. Wholly owned
/// by junto: regenerated on every init re-run, so the convention text never
/// goes stale per-repo. The user's CLAUDE.md carries only the import line.
fn write_convention(repo: &Path, channel: &str) -> Result<()> {
    let path = repo.join(CONVENTION_FILE);
    let body = format!(
        "<!-- Generated by `junto init` — do not edit; re-running init refreshes this file. -->\n\
         \n\
         ## junto — decision memory (agents, follow this)\n\
         \n\
         This repo records its development decisions in the junto channel **`{channel}`**. \
         A SessionStart hook injects the channel brief into your context; humans read and \
         verify it at `http://127.0.0.1:1727/channels/{channel}` (start the host with \
         `junto serve` if it isn't running).\n\
         \n\
         **Consult before deciding, record after:** before making or recommending a decision \
         in territory the brief covers, check it — do not contradict a **ratified** entry or \
         re-try a **parked** dead-end without surfacing it to your operator first. After a \
         consequential decision, `record` it via the junto MCP tools with real rationale and \
         provenance (PR/issue links, file paths); propose consequential *actions* through a \
         gate (`propose`) for a human to approve. Author as **yourself** (your own name/email, \
         kind agent), never as your operator, and pass your member code — the brief's last \
         line — as `code` on every write. Sync with `sync_channel` (remote `origin`) after \
         recording.\n"
    );
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    println!("wrote the consult/record convention ({CONVENTION_FILE})");
    Ok(())
}

/// Ensure the repo's CLAUDE.md imports the convention file (one stable
/// `@`-line — Claude Code expands imports at session start with the same
/// standing-instruction weight as inline text). Creates CLAUDE.md if absent;
/// appends once; never touches existing content.
fn wire_claude_md_import(repo: &Path) -> Result<()> {
    let path = repo.join("CLAUDE.md");
    let import_line = format!("@{CONVENTION_FILE}");
    let existing = if path.exists() {
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?
    } else {
        String::new()
    };
    if existing.contains(&import_line) {
        return Ok(());
    }
    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    if !updated.is_empty() {
        updated.push('\n');
    }
    updated.push_str(&format!("{import_line}\n"));
    std::fs::write(&path, updated).with_context(|| format!("writing {}", path.display()))?;
    println!("wired the convention import into CLAUDE.md ({import_line})");
    Ok(())
}

/// Tell the founder their member code once (`docs/adr/0017`) — writes through
/// the code-checked surfaces (MCP tools, web forms) will require it.
pub fn print_founder_code(opened: &host::OpenedChannel) {
    if opened.founder_code.newly_minted {
        println!(
            "your member code is {} — writes on the MCP/web surfaces require it \
             (machine-local, never in the ledger; docs/adr/0017)",
            opened.founder_code.code
        );
    } else {
        println!("your existing member code applies to this channel too");
    }
}

/// Add junto's server entry to `.mcp.json`, preserving everything else.
fn wire_mcp_json(repo: &Path) -> Result<()> {
    let path = repo.join(".mcp.json");
    let mut root: serde_json::Value = if path.exists() {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?
    } else {
        serde_json::json!({})
    };
    let servers = root
        .as_object_mut()
        .context(".mcp.json is not a JSON object")?
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    let servers = servers
        .as_object_mut()
        .context(".mcp.json mcpServers is not a JSON object")?;
    if servers.contains_key("junto") {
        return Ok(());
    }
    servers.insert(
        "junto".to_string(),
        serde_json::json!({ "type": "http", "url": MCP_URL }),
    );
    std::fs::write(&path, format!("{}\n", serde_json::to_string_pretty(&root)?))
        .with_context(|| format!("writing {}", path.display()))?;
    println!("wired the junto MCP server into .mcp.json");
    Ok(())
}

/// Add the SessionStart recall hook to `.claude/settings.json`, preserving
/// everything else.
fn wire_session_start_hook(repo: &Path) -> Result<()> {
    let dir = repo.join(".claude");
    let path = dir.join("settings.json");
    let mut root: serde_json::Value = if path.exists() {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?
    } else {
        serde_json::json!({})
    };

    let session_start = root
        .as_object_mut()
        .context("settings.json is not a JSON object")?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .context("settings.json hooks is not a JSON object")?
        .entry("SessionStart")
        .or_insert_with(|| serde_json::json!([]));
    let session_start = session_start
        .as_array_mut()
        .context("settings.json hooks.SessionStart is not an array")?;

    let already_wired = session_start.iter().any(|matcher| {
        matcher["hooks"]
            .as_array()
            .is_some_and(|hooks| hooks.iter().any(|h| h["command"] == BRIEF_COMMAND))
    });
    if already_wired {
        return Ok(());
    }
    session_start.push(serde_json::json!({
        "hooks": [{ "type": "command", "command": BRIEF_COMMAND, "timeout": 15 }]
    }));

    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::write(&path, format!("{}\n", serde_json::to_string_pretty(&root)?))
        .with_context(|| format!("writing {}", path.display()))?;
    println!("wired the SessionStart recall hook into .claude/settings.json");
    Ok(())
}

/// Keep the per-checkout binding out of the record.
fn gitignore_local_binding(repo: &Path) -> Result<()> {
    let path = repo.join(".gitignore");
    let existing = if path.exists() {
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?
    } else {
        String::new()
    };
    if existing
        .lines()
        .any(|line| line.trim() == binding::LOCAL_BINDING)
    {
        return Ok(());
    }
    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(binding::LOCAL_BINDING);
    updated.push('\n');
    std::fs::write(&path, updated).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;

    fn git_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            StdCommand::new("git")
                .args(["init", "-q"])
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success()
        );
        for (key, value) in [
            ("user.name", "Test User"),
            ("user.email", "test@example.com"),
        ] {
            assert!(
                StdCommand::new("git")
                    .args(["config", key, value])
                    .current_dir(dir.path())
                    .status()
                    .unwrap()
                    .success()
            );
        }
        dir
    }

    /// Serializes JUNTO_HOME tests: the env var is process-global, and cargo
    /// runs tests in parallel threads.
    static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Point JUNTO_HOME at a temp dir for the duration of a test.
    struct HomeGuard {
        _dir: tempfile::TempDir,
        _lock: std::sync::MutexGuard<'static, ()>,
    }
    impl HomeGuard {
        fn new() -> Self {
            let lock = HOME_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let dir = tempfile::tempdir().unwrap();
            unsafe { std::env::set_var("JUNTO_HOME", dir.path()) };
            Self {
                _dir: dir,
                _lock: lock,
            }
        }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            unsafe { std::env::remove_var("JUNTO_HOME") };
        }
    }

    #[tokio::test]
    async fn init_is_idempotent_and_wires_everything() {
        let _home = HomeGuard::new();
        let repo = git_repo();

        run(repo.path(), Some("my-channel".into()), true, None)
            .await
            .unwrap();
        // Second run must not duplicate or fail (the channel is already open,
        // but init only opens when asked).
        run(repo.path(), Some("my-channel".into()), false, None)
            .await
            .unwrap();

        // The harness wiring exists and is not duplicated.
        let mcp: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(repo.path().join(".mcp.json")).unwrap())
                .unwrap();
        assert_eq!(mcp["mcpServers"]["junto"]["url"], MCP_URL);

        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(repo.path().join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        let hooks = settings["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(hooks.len(), 1);

        // The binding names the channel; the local binding is ignored.
        assert_eq!(
            binding::bound_channels(repo.path()).unwrap(),
            vec!["my-channel".to_string()]
        );
        let gitignore = std::fs::read_to_string(repo.path().join(".gitignore")).unwrap();
        assert!(gitignore.contains(binding::LOCAL_BINDING));

        // The substrate is registered, and the channel was opened with the
        // git user as the opener.
        let junto_home = host::junto_home().unwrap();
        let substrates = host::registered_substrates(&junto_home).unwrap();
        assert_eq!(substrates.len(), 1);

        let host = host::Host::from_registry(junto_home);
        let inventory = host.inventory().await.unwrap();
        assert_eq!(inventory.len(), 1);
        assert_eq!(inventory[0].name.as_deref(), Some("my-channel"));
    }

    #[tokio::test]
    async fn init_preserves_existing_harness_config() {
        let _home = HomeGuard::new();
        let repo = git_repo();
        std::fs::write(
            repo.path().join(".mcp.json"),
            r#"{ "mcpServers": { "other": { "type": "stdio" } } }"#,
        )
        .unwrap();

        run(repo.path(), None, false, None).await.unwrap();

        let mcp: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(repo.path().join(".mcp.json")).unwrap())
                .unwrap();
        assert!(
            mcp["mcpServers"]["other"].is_object(),
            "existing servers kept"
        );
        assert!(mcp["mcpServers"]["junto"].is_object(), "junto added beside");
    }

    #[tokio::test]
    async fn init_writes_the_convention_and_one_import_line() {
        let _home = HomeGuard::new();
        let repo = git_repo();
        std::fs::write(
            repo.path().join("CLAUDE.md"),
            "# my project\n\nexisting instructions\n",
        )
        .unwrap();

        run(repo.path(), Some("conv".into()), false, None)
            .await
            .unwrap();
        // Re-run: the import line must not duplicate.
        run(repo.path(), Some("conv".into()), false, None)
            .await
            .unwrap();

        let convention = std::fs::read_to_string(repo.path().join(CONVENTION_FILE)).unwrap();
        assert!(convention.contains("channel **`conv`**"), "{convention}");
        assert!(convention.contains("Consult before deciding"));

        let claude_md = std::fs::read_to_string(repo.path().join("CLAUDE.md")).unwrap();
        assert!(
            claude_md.contains("existing instructions"),
            "user prose preserved"
        );
        assert_eq!(
            claude_md.matches(&format!("@{CONVENTION_FILE}")).count(),
            1,
            "exactly one import line: {claude_md}"
        );
    }

    #[tokio::test]
    async fn init_creates_claude_md_when_absent_and_refreshes_the_convention() {
        let _home = HomeGuard::new();
        let repo = git_repo();

        run(repo.path(), Some("first-name".into()), false, None)
            .await
            .unwrap();
        // A re-run with a different channel binding refreshes the
        // junto-owned file in place.
        run(repo.path(), Some("second-name".into()), false, None)
            .await
            .unwrap();

        let claude_md = std::fs::read_to_string(repo.path().join("CLAUDE.md")).unwrap();
        assert!(claude_md.contains(&format!("@{CONVENTION_FILE}")));
        let convention = std::fs::read_to_string(repo.path().join(CONVENTION_FILE)).unwrap();
        assert!(
            convention.contains("channel **`second-name`**"),
            "regenerated, not stale: {convention}"
        );
    }

    #[tokio::test]
    async fn init_with_agent_grants_membership_and_writes_the_relay() {
        let _home = HomeGuard::new();
        let repo = git_repo();

        run(
            repo.path(),
            Some("wedge".into()),
            true,
            Some(junto_kernel::Member::agent(
                "Claude Code",
                "claude@junto.invalid",
            )),
        )
        .await
        .unwrap();

        // The agent is in the Party (founder first).
        let host = host::Host::from_registry(host::junto_home().unwrap());
        let host::Resolution::Resolved { ledger, id, .. } = host.resolve("wedge").await.unwrap()
        else {
            panic!("channel resolves");
        };
        let view = ledger.lock().await.project(&id).await.unwrap();
        assert_eq!(view.party.len(), 2);
        assert_eq!(view.party[1].email, "claude@junto.invalid");

        // The relay carries the agent's minted code.
        let relayed = binding::local_member_code(repo.path()).unwrap();
        let on_file = crate::members::minted_members(&host::junto_home().unwrap())
            .unwrap()
            .into_iter()
            .find(|record| record.member.email == "claude@junto.invalid")
            .expect("agent minted");
        assert_eq!(relayed.as_deref(), Some(on_file.code.as_str()));
    }
}
