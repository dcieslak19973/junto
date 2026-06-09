# jüntö

> *(it's just "junto" — the dots are for fun. Everything you type — repo, package, commands — is plain `junto`.)*

**One surface where people and agents take a piece of work to a verified, provenance-bound outcome — over the tools you already use.**

junto is a place for humans and AI agents to *do work together*: talk it through, let agents act, gate the consequential steps, and end with a trustworthy, reproducible record of what happened and why.

> **Status:** early implementation. The design corpus is settled enough to build on, and the first kernel slices exist as code: the ledger (immutable entries, event-sourced projection), the gate engine, canonical serialization, and a git-refs storage backend — built in the open, MIT-licensed, decisions recorded as ADRs in [`docs/adr/`](docs/adr/). No usable product surface yet. Expect things to change.

---

## The problem

Agents made *producing* work cheap — code, analyses, changes, fixes. But cheap production moved the bottleneck somewhere else:

- **Alignment.** Agents run on unshared, local plans. By the time a pull request (or a result, or an action) shows up, it's the *first* moment anyone could course-correct — and that's too late. The hard part isn't typing the code; it's agreeing on *what* to do and *whether it's right*.
- **Trust.** Agent output is fast and abundant but not automatically trustworthy or reproducible. Findings go stale, "why" lives in people's heads, decisions aren't bound to the evidence that produced them.
- **Scatter.** The conversation is in Slack, the work item in Jira, the change in GitHub. Context is smeared across tabs.

## What junto is

The unit is a **channel** — one workspace for one piece of work, fusing the conversation, the work itself, the people *and agents* involved, the gates it must pass, and the durable record at the end. (Think: a chat channel welded to the work item it's about.)

Every channel runs the same loop:

```
  deliberate  →  agent-augmented work  →  gate  →  verified record
```

- **One surface.** External systems are pulled *in* (chat, tickets, code), so you work in one place instead of juggling apps.
- **Terminal-less.** People interact with the work, the conversation, and the artifacts — never a raw shell. Agents still run commands under the hood; their output comes back as reviewable **artifacts**, not scrollback.
- **A trustworthy record.** Consequential steps pass a gate; the result is bound to the inputs that produced it (re-runnable, auditable) — not just prose.

## It's not just for code

A channel is a **unit of inquiry** — a question — and a code change is only *one* possible outcome. The same loop handles different **kinds of work** ("Playbooks"); each playbook keeps the loop but changes the *lifecycle* (its stages, gates, tools, and output):

| Playbook | The question | Ends in |
|---|---|---|
| **code change** | "should we build/change X?" | a reviewed pull request |
| **research / analysis** | "what's actually true about X?" | a memo / result, guarded against false-discovery |
| **production troubleshooting** | "why did X break — and what do we safely do?" | a fix + an after-action record |
| **self-improvement** | "should we change our own agents/skills?" | a promoted, eval-checked change — *junto applied to itself* |

## What makes it different

- **One surface, across vendors** — not locked to one git host or one cloud.
- **Vendor-neutral by integration** — pluggable git forges (GitHub/GitLab/Bitbucket), agent harnesses (Claude Code/Codex/Goose/…), where agents run (local/WSL/SSH/remote), chat (Slack/Discord/…), and trackers (Jira/Linear). Behavior keys off *capabilities*, not vendor names.
- **Terminal-less** human surface.
- **Workflow-general** — built for many kinds of work, not only coding.
- **A verified, reproducible record** as the durable output.

It shares one good idea with GitHub Next's **Ace** — the *channel* — but Ace is GitHub-locked, cloud-fixed, and terminal-centric; junto is vendor-neutral, deployment-flexible, and terminal-less.

## Why "junto"?

Benjamin Franklin's **Junto** (Philadelphia, 1727) was a club of working tradespeople — a printer, a surveyor, a cabinetmaker, a clerk — who met to get better at their work and improve their community, pledged mutual respect across background, and ran on a standing set of questions. Their "crawling together" outlasted the club itself. A *junto* is a small, diverse group that gets better at its work by thinking together, in good faith — a fitting name for a system where some of those members are agents. (The history is the naming hook, not a spec.)

## Status & scope

- **Early implementation.** The generic kernel is underway in Rust (`crates/junto-kernel`): ledger entries with provenance, the event-sourced gate engine, a canonical (JCS) record format, and a git-refs substrate that stores the durable record in `refs/junto/*`. Syncing that record through a forge, and any human-facing surface, come next. MIT-licensed, greenfield.
- First focus: **OSS / small teams** — the durable record syncs through the git forge you already use; a heavier self-hosted/regulated mode is a later concern.
- Deliberately *not* trying to be everything at once: build a few concrete Playbooks first, extract the general framework only after.

## Design docs

This README is the front door; the thinking lives in:

- [`docs/junto.md`](docs/junto.md) — the vision spine (start here)
- [`docs/domain-model.md`](docs/domain-model.md) — the ubiquitous language: nouns & verbs
- [`docs/architecture.md`](docs/architecture.md) — substrate, sync, and the channel/governance design
- [`docs/pluggability.md`](docs/pluggability.md) — the vendor-neutral adapter boundaries
- [`docs/self-improving-harness.md`](docs/self-improving-harness.md) — the self-improvement Playbook
- `docs/worked-example-*.md` — three kinds of work walked end-to-end

## License

[MIT](LICENSE) © 2026 Dan Cieslak
