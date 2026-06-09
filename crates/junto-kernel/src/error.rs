//! Kernel error type.
//!
//! Library crates use `thiserror` and return [`Result`] — never `unwrap`,
//! `expect`, or `panic!` in non-test code (see CLAUDE.md).

/// Errors originating in the junto kernel.
///
/// Intentionally small for now; variants grow as the kernel does.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A kernel invariant was violated — a placeholder until real variants land.
    #[error("kernel invariant violated: {0}")]
    Invariant(String),
}

/// The kernel's `Result` alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;
