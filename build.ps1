# Build OpenAirCast (release) and copy it to dist\OpenAirCast.exe.
# Fails unless the embedded asset tests ran successfully in the same invocation.

$ErrorActionPreference = "Stop"
$root = $PSScriptRoot
$cargo = if (Get-Command cargo -ErrorAction SilentlyContinue) { (Get-Command cargo).Source } else { Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe" }

Write-Host "Running homepod-cast tests (includes embedded font/icon assertions)..."
Push-Location $root
try {
    & $cargo test -p homepod-cast ui::theme::tests::embedded_assets_are_present_and_icon_has_required_sizes -- --exact
    if ($LASTEXITCODE -ne 0) { throw "asset tests failed ($LASTEXITCODE)" }

    Write-Host "Building OpenAirCast (release)..."
    & $cargo build -p homepod-cast --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
} finally {
    Pop-Location
}

$dist = Join-Path $root "dist"
New-Item -ItemType Directory -Force -Path $dist | Out-Null
$dest = Join-Path $dist "OpenAirCast.exe"
Copy-Item (Join-Path $root "target\release\openaircast.exe") $dest -Force
Write-Host "Done -> $dest ($([math]::Round((Get-Item $dest).Length/1MB,2)) MB)"
