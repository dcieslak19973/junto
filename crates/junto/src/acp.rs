//! Driving a coding-agent harness over **ACP** (Agent Client Protocol) — the
//! primary harness path, with the `claude -p` CLI as fallback (`docs/adr/0024`).
//!
//! junto speaks ACP (newline-delimited JSON-RPC over the adapter's stdio) to a
//! per-harness ACP adapter — for Claude, `@agentclientprotocol/claude-agent-acp`,
//! which runs Claude Code's SDK (same subscription auth as `claude -p`, **no API
//! token**). One client, many harnesses: junto branches on the capability flags
//! ACP returns, not vendor names (CLAUDE.md constraint #4).
//!
//! This is a deliberately minimal hand-rolled client — the wire surface junto
//! needs is three requests (`initialize`, `session/new` or `session/load`,
//! `session/prompt`) and the `session/update` notification stream. The typed
//! `agent-client-protocol` crate is the upgrade path if this grows.

use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout};

use junto_kernel::EntryId;

use crate::launch::{LiveEvent, LiveSessions, TURN_TIMEOUT, TurnOutcome};
use crate::persona::McpServer;

/// A persona's config as it crosses into one ACP turn
/// (`docs/superpowers/specs/2026-06-13-agent-personas-design.md`). `mcp_servers`
/// is standard ACP (`session/new` `mcpServers`) and applies to any harness;
/// `system_prompt` and `model` ride the Claude adapter's `_meta` extensions and
/// are only populated for Claude personas (the caller gates them on harness).
#[derive(Default)]
pub(crate) struct AcpPersona {
    /// MCP servers to offer the agent.
    pub(crate) mcp_servers: Vec<McpServer>,
    /// The role / system-prompt (Claude only) → `_meta.systemPrompt`.
    pub(crate) system_prompt: Option<String>,
    /// A model override (Claude only) → `_meta.claudeCode.options.model`.
    pub(crate) model: Option<String>,
}

impl AcpPersona {
    /// The `session/new` `mcpServers` array — one `{type:"http", name, url}`
    /// element per server (the shape the adapter expects for HTTP servers).
    fn mcp_json(&self) -> Value {
        Value::Array(
            self.mcp_servers
                .iter()
                .map(|server| json!({ "type": "http", "name": server.name, "url": server.url }))
                .collect(),
        )
    }

    /// The `_meta` object for `session/new`, or `None` when the persona carries
    /// no Claude-adapter extras. Builds `systemPrompt` and
    /// `claudeCode.options.model` only as each is present.
    fn meta_json(&self) -> Option<Value> {
        if self.system_prompt.is_none() && self.model.is_none() {
            return None;
        }
        let mut meta = serde_json::Map::new();
        if let Some(prompt) = &self.system_prompt {
            meta.insert("systemPrompt".to_string(), json!(prompt));
        }
        if let Some(model) = &self.model {
            meta.insert(
                "claudeCode".to_string(),
                json!({ "options": { "model": model } }),
            );
        }
        Some(Value::Object(meta))
    }
}

/// Run one ACP turn: spawn the adapter, handshake, start (`session/new`) or
/// resume (`session/load`) a session, prompt, and stream updates into the live
/// feed. `Err` means ACP could not be set up — the caller falls back to the
/// CLI; a turn that ran but the agent failed returns `Ok` with `failed: true`.
pub(crate) async fn run_turn_acp(
    adapter: &[String],
    workspace: &Path,
    prompt: &str,
    resume: Option<&str>,
    live: &LiveSessions,
    session: EntryId,
    persona: &AcpPersona,
) -> Result<TurnOutcome> {
    let (program, args) = adapter.split_first().context("empty ACP adapter command")?;
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    // The adapter launches Claude Code's SDK; strip the nesting guard so it runs
    // even when junto was itself started from a Claude Code session.
    command.env_remove("CLAUDECODE");
    // Terminal-less: no flashed console window for the adapter process.
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

    let mut child = command.spawn().context("spawning the ACP adapter")?;
    let mut stdin = child.stdin.take().context("ACP adapter stdin")?;
    let stdout = child.stdout.take().context("ACP adapter stdout")?;
    let mut reader = BufReader::new(stdout).lines();
    let cwd = workspace.display().to_string();

    // The whole exchange runs under the turn timeout; on timeout the future
    // drops and kill_on_drop reaps the adapter (and its Claude Code child).
    let exchange = async {
        // 1. initialize
        request(
            &mut stdin,
            1,
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": { "readTextFile": false, "writeTextFile": false },
                    "terminal": false
                },
                "clientInfo": { "name": "junto", "version": env!("CARGO_PKG_VERSION") }
            }),
        )
        .await?;
        let mut sink = String::new();
        pump_until(&mut reader, &mut stdin, 1, live, session, &mut sink)
            .await
            .context("ACP initialize")?;

        // 2. session: resume (steer) or new (launch)
        let session_id = match resume {
            Some(prior) => {
                request(
                    &mut stdin,
                    2,
                    "session/load",
                    json!({ "sessionId": prior, "cwd": cwd, "mcpServers": persona.mcp_json() }),
                )
                .await?;
                pump_until(&mut reader, &mut stdin, 2, live, session, &mut sink)
                    .await
                    .context("ACP session/load")?;
                prior.to_string()
            }
            None => {
                // The persona's config rides session/new: mcpServers (standard
                // ACP) plus, for Claude personas, the adapter's _meta extras
                // (systemPrompt, claudeCode.options.model).
                let mut params = serde_json::Map::new();
                params.insert("cwd".to_string(), json!(cwd));
                params.insert("mcpServers".to_string(), persona.mcp_json());
                if let Some(meta) = persona.meta_json() {
                    params.insert("_meta".to_string(), meta);
                }
                request(&mut stdin, 2, "session/new", Value::Object(params)).await?;
                let result = pump_until(&mut reader, &mut stdin, 2, live, session, &mut sink)
                    .await
                    .context("ACP session/new")?;
                result
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .context("session/new returned no sessionId")?
                    .to_string()
            }
        };

        // 3. prompt — updates stream into `answer` while we await the response.
        request(
            &mut stdin,
            3,
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": prompt }]
            }),
        )
        .await?;
        let mut answer = String::new();
        let result = pump_until(&mut reader, &mut stdin, 3, live, session, &mut answer)
            .await
            .context("ACP session/prompt")?;

        let stop = result
            .get("stopReason")
            .and_then(|v| v.as_str())
            .unwrap_or("end_turn");
        Ok::<TurnOutcome, anyhow::Error>(TurnOutcome {
            result: if answer.trim().is_empty() {
                format!("(agent produced no message; stop reason: {stop})")
            } else {
                answer
            },
            harness_session: Some(session_id),
            failed: stop != "end_turn",
        })
    };

    match tokio::time::timeout(TURN_TIMEOUT, exchange).await {
        Ok(Ok(outcome)) => Ok(outcome),
        // A setup/handshake error → bubble up so the caller can fall back.
        Ok(Err(err)) => Err(err),
        // A timeout is a real overran turn, not a setup failure — not a fallback.
        Err(_) => Ok(TurnOutcome {
            result: format!(
                "turn exceeded the {}-minute timeout and was killed (docs/adr/0023)",
                TURN_TIMEOUT.as_secs() / 60
            ),
            harness_session: None,
            failed: true,
        }),
    }
}

/// Write a JSON-RPC request line.
async fn request(stdin: &mut ChildStdin, id: i64, method: &str, params: Value) -> Result<()> {
    write_message(
        stdin,
        &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
    )
    .await
    .with_context(|| format!("writing ACP request {method}"))
}

async fn write_message(stdin: &mut ChildStdin, message: &Value) -> Result<()> {
    let mut line = serde_json::to_string(message)?;
    line.push('\n');
    stdin.write_all(line.as_bytes()).await?;
    stdin.flush().await?;
    Ok(())
}

/// Read adapter output until the response for `awaited_id` arrives, returning
/// its `result`. Meanwhile it publishes `session/update`s into the live feed
/// (accumulating the agent's message text into `answer`) and auto-answers
/// agent→client requests (permission prompts → allow — junto's gates are the
/// outcome layer, `docs/adr/0023`).
async fn pump_until(
    reader: &mut Lines<BufReader<ChildStdout>>,
    stdin: &mut ChildStdin,
    awaited_id: i64,
    live: &LiveSessions,
    session: EntryId,
    answer: &mut String,
) -> Result<Value> {
    let mut pending = String::new();
    while let Some(line) = reader.next_line().await.context("reading ACP output")? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            continue; // tolerate any stray non-JSON line
        };
        let method = message.get("method").and_then(|m| m.as_str());
        match method {
            // A response to one of our requests (responses carry no method).
            None => {
                if message.get("id").and_then(|v| v.as_i64()) == Some(awaited_id) {
                    flush_pending(&mut pending, live, session);
                    if let Some(error) = message.get("error") {
                        bail!("ACP error on request {awaited_id}: {error}");
                    }
                    return Ok(message.get("result").cloned().unwrap_or(Value::Null));
                }
            }
            Some("session/update") => {
                if let Some(update) = message.get("params").and_then(|p| p.get("update")) {
                    handle_update(update, live, session, answer, &mut pending);
                }
            }
            // An agent→client request (has a non-null id) — answer it.
            Some(other) => {
                if let Some(id) = message.get("id").filter(|id| !id.is_null()) {
                    let result = answer_agent_request(other, message.get("params"));
                    write_message(
                        stdin,
                        &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                    )
                    .await?;
                }
            }
        }
    }
    bail!("ACP adapter closed before responding to request {awaited_id}")
}

/// Map one `session/update` into live-feed events; accumulate agent text into
/// `answer` (the final memo) and flush readable lines via `pending`.
fn handle_update(
    update: &Value,
    live: &LiveSessions,
    session: EntryId,
    answer: &mut String,
    pending: &mut String,
) {
    match update.get("sessionUpdate").and_then(|v| v.as_str()) {
        Some("agent_message_chunk") => {
            if let Some(text) = update.pointer("/content/text").and_then(|v| v.as_str()) {
                answer.push_str(text);
                pending.push_str(text);
                // Flush whole lines to the feed so it streams readably without a
                // per-token <li> storm.
                while let Some(newline) = pending.find('\n') {
                    let line: String = pending.drain(..=newline).collect();
                    let line = line.trim_end();
                    if !line.is_empty() {
                        live.publish(session, LiveEvent::new("assistant", line));
                    }
                }
            }
        }
        Some("tool_call") => {
            let label = update
                .get("title")
                .and_then(|v| v.as_str())
                .or_else(|| update.pointer("/rawInput/command").and_then(|v| v.as_str()))
                .unwrap_or("tool");
            let label: String = label
                .lines()
                .next()
                .unwrap_or(label)
                .chars()
                .take(80)
                .collect();
            live.publish(session, LiveEvent::new("tool", label));
        }
        // usage_update, plan, agent_thought_chunk, available_commands_update,
        // tool_call_update: not surfaced in the v1 feed.
        _ => {}
    }
}

/// Flush any buffered (newline-less) agent text as a final feed line.
fn flush_pending(pending: &mut String, live: &LiveSessions, session: EntryId) {
    let tail = pending.trim();
    if !tail.is_empty() {
        live.publish(session, LiveEvent::new("assistant", tail));
    }
    pending.clear();
}

/// The result for an agent→client request. Permission prompts are auto-allowed
/// (yolo, `docs/adr/0023`); anything else gets an empty ack.
fn answer_agent_request(method: &str, params: Option<&Value>) -> Value {
    if method != "session/request_permission" {
        return json!({});
    }
    let options = params
        .and_then(|p| p.get("options"))
        .and_then(|o| o.as_array());
    // Prefer an explicit "allow" option; otherwise take the first offered.
    let option_id = options
        .and_then(|opts| {
            opts.iter().find_map(|opt| {
                let id = opt.get("optionId").and_then(|v| v.as_str())?;
                let kind = opt.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                (id.contains("allow") || kind.contains("allow")).then(|| id.to_string())
            })
        })
        .or_else(|| {
            options
                .and_then(|opts| opts.first())
                .and_then(|opt| opt.get("optionId"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "allow".to_string());
    json!({ "outcome": { "outcome": "selected", "optionId": option_id } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_json_is_the_http_server_shape_the_adapter_expects() {
        let persona = AcpPersona {
            mcp_servers: vec![McpServer {
                name: "junto".into(),
                url: "http://127.0.0.1:1727/mcp".into(),
            }],
            ..Default::default()
        };
        assert_eq!(
            persona.mcp_json(),
            json!([{ "type": "http", "name": "junto", "url": "http://127.0.0.1:1727/mcp" }])
        );
    }

    #[test]
    fn meta_json_builds_only_the_present_claude_extras() {
        // No extras → no _meta at all.
        assert!(AcpPersona::default().meta_json().is_none());
        // systemPrompt and model land under the adapter's recognized keys.
        let persona = AcpPersona {
            system_prompt: Some("be careful".into()),
            model: Some("claude-opus-4-8".into()),
            ..Default::default()
        };
        assert_eq!(
            persona.meta_json(),
            Some(json!({
                "systemPrompt": "be careful",
                "claudeCode": { "options": { "model": "claude-opus-4-8" } }
            }))
        );
    }
}
