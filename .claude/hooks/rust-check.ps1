# PostToolUse hook: auto-format and lint Rust after edits.
# Fires only when the edited file is a .rs file. Formats (fast, idempotent),
# then runs clippy and surfaces any failures back to the agent (exit 2).
$ErrorActionPreference = 'Stop'

$raw = [Console]::In.ReadToEnd()
try { $payload = $raw | ConvertFrom-Json } catch { exit 0 }

$path = $payload.tool_input.file_path
if (-not $path -or ($path -notmatch '\.rs$')) { exit 0 }

$root = if ($env:CLAUDE_PROJECT_DIR) { $env:CLAUDE_PROJECT_DIR } else { 'D:\git\junto' }
Set-Location $root

# Auto-format the workspace (cheap, idempotent).
& cargo fmt --quiet

# Lint; if clippy is unhappy, hand its output back to the agent.
$clippy = & cargo clippy --workspace --all-targets --quiet -- -D warnings 2>&1
if ($LASTEXITCODE -ne 0) {
    [Console]::Error.WriteLine("cargo clippy found issues (fix before continuing):`n$clippy")
    exit 2
}
exit 0
