---
name: dead-ends
description: Check junto's dead-ends (parked assertions, rejected proposals) before re-trying an approach that may have been tried. Use when proposing a design/approach in territory with prior history, when something feels like it was attempted before, or when the user asks "didn't we try this?".
---

# Surfacing dead-ends

The junto brief injected at session start carries **state, not history** — parked
dead-ends and rejected proposals are deliberately omitted from it. They surface on
demand through the `dead_ends` MCP tool.

## When to call

- Before proposing or attempting an approach in territory that may have been tried.
- When the user (or your own context) hints at prior history: "didn't we…", "again",
  "go back to…".
- When a parked path seems to be **coming back from the dead** — new evidence, changed
  constraints, a revived idea.

## How

Call `mcp__junto__dead_ends` with the channel (default here: `junto-dev`). Pass
`about` describing the approach you're considering — dead-ends come back ranked by
similarity, top few only. The match is lexical (token overlap), so if a first query
misses, try other words for the same idea before concluding the territory is untried.

## What to do with a hit

Do **not** silently re-try a listed dead-end. Surface it to the user first: quote the
park/rejection rationale and who recorded it, and ask whether the conditions that
killed it have changed. If the path genuinely revives, record that explicitly (a new
assertion citing the parked entry, or `correct` the parked one) so the record shows
the resurrection rather than an unexplained re-tread.
