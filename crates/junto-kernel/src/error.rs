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

    /// A [`LedgerEntry`](crate::LedgerEntry) could not be (de)serialized to or
    /// from its canonical byte form. The message carries the underlying cause;
    /// the concrete serializer/parser type is deliberately not exposed so the
    /// public error stays independent of the record format (see `docs/adr/0008`).
    #[error("ledger entry serialization failed: {0}")]
    Serialization(String),
}

/// The kernel's `Result` alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;
