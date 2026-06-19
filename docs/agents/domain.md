# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring the codebase.

> **junto note:** this repo's glossary is **`docs/domain-model.md`** — *not* `CONTEXT.md` (the default the seed templates assume). Wherever a skill says "read `CONTEXT.md`", read `docs/domain-model.md` here. The repo is **single-context** (one glossary + one `docs/adr/`; no `CONTEXT-MAP.md`).

## Before exploring, read these

- **`docs/domain-model.md`** at the repo root of `docs/` — the ubiquitous language (the nouns & verbs). `CLAUDE.md` already requires reading it before naming types.
- **`docs/adr/`** — read the ADRs that touch the area you're about to work in. `docs/adr/README.md` is the index (one line per decision); ADR `Status:` lines carry the actual decision date, and cross-references point backward by number.

If something you expect isn't there, **proceed silently** — don't flag absence or suggest creating files upfront. The producer skill (`grill-with-docs`) maintains `docs/domain-model.md` and `docs/adr/` lazily, as terms and decisions actually get resolved.

## File structure (single-context)

```
/
├── CLAUDE.md
└── docs/
    ├── domain-model.md          ← the glossary (ubiquitous language)
    ├── adr/
    │   ├── README.md            ← the ADR index
    │   ├── 0001-….md
    │   └── …                    ← one settled decision per file
    └── … (junto.md, architecture.md, attention.md, pluggability.md, …)
```

## Use the glossary's vocabulary

When your output names a domain concept (an issue title, a refactor proposal, a hypothesis, a test name, a type), use the term as defined in `docs/domain-model.md`. Don't drift to synonyms the glossary explicitly avoids (e.g. it is `Channel` / `Gate` / `LedgerEntry`; the diverge/converge verbs are settled — never "fork").

If the concept you need isn't in the glossary yet, that's a signal — either you're inventing language the project doesn't use (reconsider) or there's a real gap (note it for `grill-with-docs`).

## Flag ADR conflicts

If your output contradicts an existing ADR, surface it explicitly rather than silently overriding:

> _Contradicts ADR-0027 (channel lineage is diverge/converge edge entries) — but worth reopening because…_
