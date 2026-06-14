# Align junto's terminology on Anthropic's Managed Agents

Status: accepted (Dan, 2026-06-14) · **supersedes the naming** in [`0007`](0007-routing-stays-out-of-the-kernel.md) (the routing layer is no longer "Rubric") and [`0020`](0020-agent-sessions-and-artifacts-are-ledger-entries.md) (drops the "Agent Session" qualifier) · **renames** the Persona concept (ledger `251c4bba`) → Agent · informed by Anthropic's **Managed Agents** docs ([define-outcomes](https://platform.claude.com/docs/en/managed-agents/define-outcomes), [agent-setup](https://platform.claude.com/docs/en/managed-agents/agent-setup))

junto adopts **Anthropic's Managed Agents vocabulary** as its ubiquitous language wherever the two overlap — including where Anthropic's usage *contradicts* a previously-settled junto noun. The bet (Dan): Anthropic is setting the de-facto vocabulary for agent systems, and a vendor-neutral product still benefits from speaking the words its users already know. The cost — churning settled kernel nouns and a just-shipped concept — is accepted and paid once, up front, as a dedicated rename PR **before** the first Playbook is built (so new code is born in the final language).

This is a **naming** decision. It changes no kernel mechanics: the Ledger, the Gate engine, projection, the substrate, sync, and the entry envelope are untouched. Only the words move.

## The mapping

| Anthropic term | Means | junto **before** | junto **after** |
|---|---|---|---|
| **Agent** | a reusable, versioned config (model · system · tools · MCP · skills) | **Persona** (machine-local config over a harness) | **Agent** (the config) |
| **Session** | one unit of agent work | **Agent Session** (always qualified) | **Session** |
| **Outcome** | *the target* — "what done looks like" (description + rubric) | — | **Outcome** = the target/goal definition |
| (deliverable / output files) | what the agent produced | **Outcome** (PR · memo · fix · parked) | **Deliverable** |
| **Rubric** | gradeable verification criteria (markdown) | "Rubric" = the **routing** layer (ADR 0007) | **Rubric** = verification criteria |
| **Grader** | clean-context evaluator of a deliverable vs a rubric | — | **Grader** |
| `max_iterations` / `needs_revision` / `satisfied` / `failed` | the grading loop's vocabulary | — | adopted as-is |

**Renamed to clear the collisions:**

- **Persona → Agent.** The config noun takes Anthropic's word. This renames the ratified concept in ledger `251c4bba` and the `persona.rs` / `personas.toml` / `channel_persona` code.
- **Outcome (produced) → Deliverable.** junto's old `Outcome` (a ✅-settled kernel noun: PR | memo | fix | parked) is renamed so **Outcome** can take Anthropic's meaning (the *target*). This is the one genuinely contradictory swap — the two words sit at opposite ends of the loop — and it is the highest-churn rename, so it is called out explicitly.
- **Agent Session → Session.** The qualifier is dropped (overriding 0020's settled-naming guard). The code already uses bare `Session` (`SessionStarted`, `SessionState`); this is mostly a docs change. The Ace-collision the qualifier guarded against is judged tolerable now (Ace's own API renamed to "Channel" in 0.1.70 — domain-model already noted the guard was weakening).
- **Routing "Rubric" → Routing Policy.** ADR 0007's future routing layer loses the name "Rubric" (now reserved for verification criteria). The routing layer is **unbuilt**, so this costs nothing but a word.

## Agent vs Member (the sub-decision)

Now that **Agent** = the *config*, a machine *participant* in a Channel's Party needs no new noun: a **Member**'s kind is `human` or `agent`, and `MemberKind::{Human, Agent}` is unchanged. The sentence that disambiguates: **"an agent Member runs an Agent."** `Agent` (capitalized, standalone) is the config; `agent` (lowercase, as a Member kind) is the participant. Members, the Party, and the founder-granted membership model (ADR 0017) are otherwise untouched.

## Verifier retires; the Verifier slot becomes Outcome + Rubric + Grader

Anthropic's model gives the per-playbook verification slot a concrete shape, so junto's standalone **Verifier** noun is **retired**. In the new language: **a Playbook supplies an Outcome definition** — mechanical checks plus a **Rubric** — and a **Grader** evaluates a Deliverable against the Rubric, in a separate context window (clean-room judgment, which is exactly junto's "verified record" thesis). The result of grading is what moves a ledger entry's verification state. "What verified means for this playbook" is now spelled *Outcome + Rubric*, not *Verifier*.

## What survives unchanged

Anthropic's Managed Agents has no equivalent to align these to, so they stay junto's: **Channel · Member · Party · Gate · Ledger / entries · Artifact · Provenance · Playbook · Substrate** (and the adapter nouns). The most important survivor is **Gate**: Anthropic's define-outcomes loop is agent *self-iteration* against a grader, with **no human-approval checkpoint**. junto's Gate — a consequential action pausing for a human — is purely junto's, and it is what makes junto a governance surface rather than an autonomy harness.

## Migration

**Big-bang rename PR, before the first Playbook.** One sweep renames code (`Persona → Agent`, `Outcome → Deliverable`, the routing "Rubric" comments) and docs (this corpus), so the code-PR Playbook is built entirely in the final vocabulary. The alternative — rename incrementally as files are touched — was rejected to avoid new feature code being born in doomed names.

## Risks & considered

- **Corrupting a deliberately-crafted ubiquitous language** (CLAUDE.md treats the glossary as load-bearing). Mitigation: this ADR + the updated `domain-model.md` are the single source of truth; the rename is mechanical and total, not partial.
- **The Outcome flip is a genuine footgun** — the same word means opposite things before and after. Mitigation: `Outcome → Deliverable` is done in the *same* PR as `target → Outcome`, never split; any lingering "outcome means the produced thing" reference is a bug to fix on sight.
- **Considered: align only the new terms** (adopt Grader + Rubric, keep Outcome/Session/Persona). Rejected by Dan in favour of full alignment — partial alignment leaves junto speaking a dialect, which defeats the point of borrowing the vocabulary at all.
- **Considered: keep junto's language, treat Anthropic's as foreign.** Rejected for the same reason: ecosystem gravity is the asset being bought.
- The **define_outcome grading loop is a Managed-Agents server-side API event**, not exposed on junto's current ACP path — so adopting the *vocabulary* does not mean junto can emit `user.define_outcome` today. Native define-outcome is a future harness path; the terms are adopted now regardless.
