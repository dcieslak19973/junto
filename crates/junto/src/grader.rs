//! The **Grader**: scores a Deliverable against a **Rubric** in a fresh,
//! clean-context Session (`docs/adr/0025`). Running the judgment in a *new*
//! harness session — not a resume of the worker's — is what keeps it
//! independent of the implementer's reasoning (Anthropic's "separate context
//! window"). This module owns the pure pieces: the built-in rubric, the prompt
//! the Grader is given, and parsing its reply into a verdict. The harness call
//! itself is glue in the Outcome loop.

/// The Grader's verdict on a Deliverable vs a Rubric.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GraderVerdict {
    /// Whether the Deliverable satisfies the Rubric.
    pub(crate) satisfied: bool,
    /// The Grader's explanation — fed back to the worker as the next turn's
    /// instructions when not satisfied.
    pub(crate) feedback: String,
}

/// The built-in code-PR **Rubric** — gradeable criteria the Grader scores a
/// Deliverable against. Overridable per-repo later (rule of three); v1 ships
/// this default so the dogfood repo grades zero-config.
pub(crate) fn default_code_pr_rubric() -> &'static str {
    "\
# Code-PR Rubric

Score each criterion independently against the diff.

## Tests
- New or changed behavior has corresponding tests.
- Existing tests were not weakened or deleted to make a change pass.

## Scope
- The diff is focused on one change; no unrelated edits or drive-by reformatting.
- No dead code, commented-out blocks, or stray debug prints left behind.

## Safety
- No secrets, credentials, API keys, or tokens are committed.
- No obvious logic errors, unhandled error paths, or panics on the happy path.

## Clarity
- Public items have doc comments; names match the surrounding code's conventions.
"
}

/// Build the prompt the Grader is given: the Rubric, the diff to judge, and the
/// instruction to end with a parseable verdict line. The Grader runs in a fresh
/// session with only this prompt — no implementer context.
pub(crate) fn grader_prompt(rubric: &str, diff: &str) -> String {
    format!(
        "You are an independent reviewer grading a code change against a rubric. \
You see only the rubric and the diff — not the author's reasoning — so judge what \
the diff actually does.\n\n\
Score each criterion against the diff below. Be concrete: name the file and the \
issue for anything that fails.\n\n\
# Rubric\n\n{rubric}\n\n\
# Diff under review\n\n```diff\n{diff}\n```\n\n\
End your reply with exactly one line — either `VERDICT: SATISFIED` if every \
criterion passes, or `VERDICT: NEEDS_REVISION` if any criterion fails. When it \
needs revision, list what must change above the verdict line.\n"
    )
}

/// Parse the Grader's reply into a verdict. **Fail-safe:** a reply without a
/// clear `VERDICT: SATISFIED` line is treated as needs-revision, so the loop
/// never passes on a judgment it couldn't read.
pub(crate) fn parse_verdict(reply: &str) -> GraderVerdict {
    // The Grader is told to end with a `VERDICT: SATISFIED | NEEDS_REVISION`
    // line; read the last line that mentions one. Anything else is needs-revision.
    let satisfied = reply
        .lines()
        .rev()
        .find(|line| line.to_ascii_uppercase().contains("VERDICT:"))
        .is_some_and(|line| {
            let upper = line.to_ascii_uppercase();
            upper.contains("SATISFIED") && !upper.contains("NEEDS")
        });
    GraderVerdict {
        satisfied,
        feedback: reply.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn satisfied_verdict_is_parsed() {
        let verdict =
            parse_verdict("Tests cover the new path; diff is focused.\nVERDICT: SATISFIED");
        assert!(verdict.satisfied);
    }

    #[test]
    fn needs_revision_verdict_is_parsed() {
        let verdict = parse_verdict("The new function has no tests.\nVERDICT: NEEDS_REVISION");
        assert!(!verdict.satisfied);
        assert!(verdict.feedback.contains("no tests"));
    }

    #[test]
    fn unrecognized_output_is_needs_revision() {
        // Fail-safe: if the grader's reply has no clear verdict, don't pass.
        let verdict = parse_verdict("I looked at the diff and have some thoughts.");
        assert!(!verdict.satisfied);
    }

    #[test]
    fn a_satisfied_line_followed_by_a_revision_verdict_is_not_satisfied() {
        // The trailing verdict line wins, not an earlier mention of "satisfied".
        let verdict = parse_verdict(
            "Criterion 1 is satisfied.\nCriterion 2 is missing tests.\nVERDICT: NEEDS_REVISION",
        );
        assert!(!verdict.satisfied);
    }

    #[test]
    fn default_rubric_covers_the_core_code_pr_criteria() {
        let rubric = default_code_pr_rubric().to_ascii_lowercase();
        assert!(!rubric.is_empty());
        assert!(rubric.contains("test"), "rubric should require tests");
        assert!(
            rubric.contains("secret") || rubric.contains("credential"),
            "rubric should flag leaked secrets"
        );
        assert!(
            rubric.contains("focused") || rubric.contains("scope"),
            "rubric should ask for a focused diff"
        );
    }

    #[test]
    fn grader_prompt_carries_rubric_diff_and_verdict_instruction() {
        let prompt = grader_prompt("RUBRIC-MARKER", "DIFF-MARKER");
        assert!(
            prompt.contains("RUBRIC-MARKER"),
            "prompt must include the rubric"
        );
        assert!(
            prompt.contains("DIFF-MARKER"),
            "prompt must include the diff"
        );
        let upper = prompt.to_ascii_uppercase();
        assert!(
            upper.contains("VERDICT: SATISFIED"),
            "must instruct the satisfied verdict"
        );
        assert!(
            upper.contains("VERDICT: NEEDS_REVISION"),
            "must instruct the needs-revision verdict"
        );
    }
}
