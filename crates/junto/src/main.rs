//! junto — the host/app entry point.
//!
//! NOTE: junto is **terminal-less for humans** (CLAUDE.md constraint #2). This
//! binary is the host *process*, not a human-facing terminal UI; any human
//! surface is served elsewhere. Binary/`main` code may use `anyhow` and may
//! `?`-propagate — unlike the library crates.

use anyhow::Result;

fn main() -> Result<()> {
    // Placeholder: proves the kernel links and the workspace builds.
    let _kernel_ready: junto_kernel::Result<()> = Ok(());
    println!("junto {} — scaffold", env!("CARGO_PKG_VERSION"));
    Ok(())
}
