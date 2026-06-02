# Cleanup of `target/` artifacts. Run when the directory has grown
# past ~10 GB (cargo's incremental + deps caches accumulate even
# with incremental=false, since old rlib versions linger when deps
# change versions).
#
# Modes:
#   - default: `cargo clean` — nukes the entire target/. Forces a
#              full rebuild next time (~4-5 min).
#   - -Stale: removes only artifacts not touched in the last 14
#              days. Keeps recent caches, drops the cruft.
#
# Usage:
#   pwsh ./scripts/clean-target.ps1            # full cargo clean
#   pwsh ./scripts/clean-target.ps1 -Stale     # stale-only cleanup

param(
    [switch]$Stale
)

$root = Split-Path -Parent $PSScriptRoot
$target = Join-Path $root 'target'

if (-not (Test-Path $target)) {
    Write-Output 'target/ does not exist; nothing to clean.'
    exit 0
}

$before = (Get-ChildItem -Recurse $target -ErrorAction SilentlyContinue | Measure-Object Length -Sum).Sum

if ($Stale) {
    $cutoff = (Get-Date).AddDays(-14)
    Write-Output "Removing target/ artifacts not touched since $($cutoff.ToString('yyyy-MM-dd'))..."
    Get-ChildItem -Recurse -File $target -ErrorAction SilentlyContinue |
        Where-Object { $_.LastWriteTime -lt $cutoff } |
        Remove-Item -Force -ErrorAction SilentlyContinue
} else {
    Write-Output 'Running cargo clean...'
    Push-Location $root
    try {
        cargo clean
    } finally {
        Pop-Location
    }
}

$after = (Get-ChildItem -Recurse $target -ErrorAction SilentlyContinue | Measure-Object Length -Sum).Sum
$freed = ($before - $after) / 1GB

Write-Output ('Freed: {0:N2} GB (before {1:N2} GB, after {2:N2} GB)' -f $freed, ($before/1GB), ($after/1GB))
