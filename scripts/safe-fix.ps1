<#
.SYNOPSIS
  Runs the F401 (unused-import) auto-fix on a target path with an automatic
  safety net: detect -> propose diff -> apply -> verify (cargo test) ->
  automatic rollback on any failure.

.DESCRIPTION
  This wraps `pylint-rs --fix`, which already does the detect/diff/apply
  steps and prints a diff before writing. This script adds the verification
  and automatic-rollback layer around it:

    1. Refuses to run if the target path has uncommitted changes, so a
       rollback has an unambiguous git baseline to return to.
    2. Builds the linter.
    3. Runs `pylint-rs --fix` (prints the diff, applies the change).
    4. If nothing changed on disk, exits cleanly - nothing to verify.
    5. If something changed, reruns the full `cargo test` suite.
    6. On test failure, runs `git checkout -- <path>` to revert the fix
       automatically, then exits non-zero.
    7. On test success, leaves the fix in place and exits zero.

  Verification and rollback live here, outside the linter binary itself,
  because "rerun tests" is inherently specific to whichever project is
  being fixed - pylint-rs is meant to lint arbitrary Python codebases, most
  of which have nothing to do with `cargo test`. This repo's own example
  fixtures are the sandbox for this prototype.

.PARAMETER Path
  File or directory to fix, relative to the repo root (e.g. examples\bad.py).

.EXAMPLE
  .\scripts\safe-fix.ps1 -Path examples\bad.py
#>
param(
    [Parameter(Mandatory = $true)]
    [string]$Path
)

$ErrorActionPreference = "Stop"
$repoRoot = "C:\Users\joshu\code\py-lint-rs"
Set-Location $repoRoot

function Write-Step($msg) { Write-Host "`n== $msg ==" -ForegroundColor Cyan }

Write-Step "Preflight: checking for a clean baseline on '$Path'"
$dirty = git status --porcelain -- $Path
if ($dirty) {
    Write-Host "Refusing to run: '$Path' already has uncommitted changes." -ForegroundColor Red
    Write-Host "Commit or stash them first so a rollback has a clean baseline." -ForegroundColor Red
    exit 2
}
Write-Host "Clean."

Write-Step "Building pylint-rs"
cargo build --release --quiet
if ($LASTEXITCODE -ne 0) {
    Write-Host "Build failed - aborting before touching any files." -ForegroundColor Red
    exit 2
}

$exe = Join-Path $repoRoot "target\release\pylint-rs.exe"

Write-Step "Detect + propose diff + apply"
& $exe --fix $Path

$changed = git status --porcelain -- $Path
if (-not $changed) {
    Write-Host "`nNo fixable unused imports found. Nothing to verify or roll back." -ForegroundColor Yellow
    exit 0
}

Write-Step "Verify: rerunning cargo test"
cargo test --quiet 2>&1 | Out-Host
$testsPassed = ($LASTEXITCODE -eq 0)

if (-not $testsPassed) {
    Write-Step "Tests failed - rolling back automatically"
    git checkout -- $Path
    Write-Host "Reverted '$Path' to its last committed state." -ForegroundColor Yellow
    Write-Host "RESULT: rolled back" -ForegroundColor Red
    exit 1
}

Write-Step "Tests passed"
Write-Host "RESULT: fix kept on '$Path'" -ForegroundColor Green
exit 0
