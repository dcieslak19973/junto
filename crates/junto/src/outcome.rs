//! The **Outcome loop** (`docs/adr/0025`'s define-outcomes / `max_iterations`
//! pattern). A worker Session does work; junto verifies it (mechanical checks +
//! the Grader); if it isn't satisfied, the findings feed back as the next turn's
//! instructions and the worker revises — until the Outcome is satisfied or the
//! iteration budget runs out.
//!
//! This module owns the pure **control logic** ([`drive_loop`]): when to stop
//! and what feedback to carry forward. The worker turn and the verify step are
//! injected, so the loop is testable without a real harness; the Outcome
//! orchestrator wires the real `run_turn` / [`crate::verify`] / [`crate::grader`]
//! steps in.

/// How the Outcome loop ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LoopTerminal {
    /// The Deliverable satisfied the Outcome within the iteration budget.
    Satisfied {
        /// How many iterations it took.
        iterations: u32,
    },
    /// The budget ran out before the Outcome was satisfied — escalate to a Gate.
    MaxIterationsReached {
        /// The budget that was exhausted.
        iterations: u32,
        /// The last round's findings, for the escalation Gate.
        last_feedback: String,
    },
}

/// The gated, ACE-style outcome a finished loop records for the future
/// self-improvement Playbook to learn from (`docs/self-improving-harness.md`):
/// **success** when the Outcome was satisfied, **partial** when the loop
/// escalated to a Gate without meeting it. (`failure` is reserved for a worker
/// that could not run, or a rejected escalation — not produced yet.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutcomeResult {
    Success,
    Partial,
    // Part of the recorded signal's success/partial/failure shape, but not yet
    // emitted: v1 has no failure terminal (a worker that can't run, or a rejected
    // escalation, will produce it). Kept so the signal schema is stable now.
    #[allow(dead_code)]
    Failure,
}

impl OutcomeResult {
    /// The wire token recorded in the outcome-signal artifact.
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            OutcomeResult::Success => "success",
            OutcomeResult::Partial => "partial",
            OutcomeResult::Failure => "failure",
        }
    }
}

impl LoopTerminal {
    /// The structured outcome signal for this terminal.
    pub(crate) fn result(&self) -> OutcomeResult {
        match self {
            LoopTerminal::Satisfied { .. } => OutcomeResult::Success,
            LoopTerminal::MaxIterationsReached { .. } => OutcomeResult::Partial,
        }
    }
}

/// One verification round's result: satisfied, or feedback to revise with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VerifyOutcome {
    /// Whether the Deliverable satisfied the Outcome this round.
    pub(crate) satisfied: bool,
    /// The findings to feed back to the worker when not satisfied.
    pub(crate) feedback: String,
}

/// Drive the loop: run the worker (with the prior round's feedback, or `None`
/// on the first turn), verify, and repeat until satisfied or `max_iterations`.
pub(crate) async fn drive_loop<Worker, Verify>(
    max_iterations: u32,
    mut worker: Worker,
    mut verify: Verify,
) -> LoopTerminal
where
    Worker: AsyncFnMut(Option<String>),
    Verify: AsyncFnMut() -> VerifyOutcome,
{
    // Feedback is passed by value (not `Option<&str>`): a borrowed argument makes
    // the async closure's `AsyncFnMut` impl not general enough to satisfy the
    // `Send + 'static` future that `tokio::spawn` requires.
    let mut feedback: Option<String> = None;
    for iteration in 1..=max_iterations {
        worker(feedback.clone()).await;
        let result = verify().await;
        if result.satisfied {
            return LoopTerminal::Satisfied {
                iterations: iteration,
            };
        }
        feedback = Some(result.feedback);
    }
    LoopTerminal::MaxIterationsReached {
        iterations: max_iterations,
        last_feedback: feedback.unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn satisfied_terminal_is_a_success_signal() {
        let terminal = LoopTerminal::Satisfied { iterations: 2 };
        assert_eq!(terminal.result(), OutcomeResult::Success);
        assert_eq!(terminal.result().as_str(), "success");
    }

    #[test]
    fn max_iterations_terminal_is_a_partial_signal() {
        let terminal = LoopTerminal::MaxIterationsReached {
            iterations: 3,
            last_feedback: "still failing".to_string(),
        };
        assert_eq!(terminal.result(), OutcomeResult::Partial);
        assert_eq!(terminal.result().as_str(), "partial");
    }

    #[tokio::test]
    async fn satisfied_on_the_first_iteration() {
        let worker_calls = Cell::new(0u32);
        let terminal = drive_loop(
            3,
            async |_feedback: Option<String>| {
                worker_calls.set(worker_calls.get() + 1);
            },
            async || VerifyOutcome {
                satisfied: true,
                feedback: String::new(),
            },
        )
        .await;
        assert_eq!(terminal, LoopTerminal::Satisfied { iterations: 1 });
        assert_eq!(worker_calls.get(), 1, "worker runs exactly once when green");
    }

    #[tokio::test]
    async fn exhausts_the_budget_when_never_satisfied() {
        let worker_calls = Cell::new(0u32);
        let terminal = drive_loop(
            3,
            async |_feedback: Option<String>| {
                worker_calls.set(worker_calls.get() + 1);
            },
            async || VerifyOutcome {
                satisfied: false,
                feedback: "still broken".to_string(),
            },
        )
        .await;
        assert_eq!(
            terminal,
            LoopTerminal::MaxIterationsReached {
                iterations: 3,
                last_feedback: "still broken".to_string(),
            }
        );
        assert_eq!(worker_calls.get(), 3, "worker runs once per iteration");
    }

    #[tokio::test]
    async fn satisfied_on_the_second_iteration() {
        let worker_calls = Cell::new(0u32);
        let verify_calls = Cell::new(0u32);
        let terminal = drive_loop(
            3,
            async |_feedback: Option<String>| {
                worker_calls.set(worker_calls.get() + 1);
            },
            async || {
                verify_calls.set(verify_calls.get() + 1);
                VerifyOutcome {
                    satisfied: verify_calls.get() == 2,
                    feedback: "fix it".to_string(),
                }
            },
        )
        .await;
        assert_eq!(terminal, LoopTerminal::Satisfied { iterations: 2 });
        assert_eq!(worker_calls.get(), 2);
    }

    #[tokio::test]
    async fn feedback_is_passed_to_the_next_worker_turn() {
        let seen_feedback: std::cell::RefCell<Vec<Option<String>>> =
            std::cell::RefCell::new(Vec::new());
        let verify_calls = Cell::new(0u32);
        drive_loop(
            3,
            async |feedback: Option<String>| {
                seen_feedback.borrow_mut().push(feedback);
            },
            async || {
                verify_calls.set(verify_calls.get() + 1);
                VerifyOutcome {
                    satisfied: verify_calls.get() == 2,
                    feedback: "add tests for foo".to_string(),
                }
            },
        )
        .await;
        let seen = seen_feedback.borrow();
        assert_eq!(seen[0], None, "first turn has no feedback");
        assert_eq!(
            seen[1],
            Some("add tests for foo".to_string()),
            "second turn receives the prior round's findings"
        );
    }
}
