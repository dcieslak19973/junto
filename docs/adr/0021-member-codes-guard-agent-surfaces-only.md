# Member codes guard agent surfaces only; the human surface authorizes by membership

Status: accepted (Dan, 2026-06-11) · refines [`0017`](0017-party-is-a-projection-membership-is-founder-granted.md) · builds on [`0012`](0012-mcp-over-http-is-the-first-write-surface.md), [`0018`](0018-human-surface-is-a-desktop-shell-over-the-host.md)

0017 said "the host's write surfaces require the member code". Decided: that requirement narrows to the **agent-facing surfaces** (MCP). The human surface (the web pages, and the desktop shell that wraps them) asks for **no code** — it checks membership only.

## Why the code is meaningless on the human surface

The code's job (0017) is accident-proofing a **claimed** identity: on MCP, an agent names its own author, and holding only its own code stops it accidentally authoring as someone else. On the human surface neither half of that applies:

- **The author is not claimed — the host derives it** from the substrate's git config. The form never chooses who you are, so there is no mis-claiming to prevent.
- **The check proves nothing the OS boundary doesn't.** The host stores the codes (`~/.junto/members.toml`) and runs as the machine user; anyone who can POST to the localhost-only port can equally read the store. Demanding a string back that the server itself holds is friction, not safety. The SSH-tunnel remote story (0018) preserves this: a tunneled connection *is* the OS boundary extended.

What remains on the human surface is the **membership** check: a git identity that was never granted membership is refused with a clear message (the alternative — recording an entry that projects as *unrecognized* — is exactly the orphaned-entry confusion 0017's guardrail exists to prevent).

## Consequences

- The act forms carry rationale only; the member-code input, the remember-cookie, and the code-retry page (slice 16's recovery flow) are deleted — the failure mode they recovered from no longer exists.
- The desktop shell needs no code plumbing at all: wrapping the pages suffices (one implementation of every pixel, 0018).
- MCP is unchanged: agents still pass `code`, wrong/missing codes are still refused.

## Considered

- **Auto-fill the code (cookie set by the desktop app, or host self-lookup) while keeping the check** — rejected: it preserves the letter of 0017 by maintaining theater; a check whose answer the checker supplies verifies nothing.
- **Dropping the code on MCP too** — rejected: there the identity is genuinely claimed, the accident is real (an agent posting as its operator was the original sin 0012 documented), and the code is the one thing distinguishing "agent holding its own credential" from "agent pasting someone else's identity".
