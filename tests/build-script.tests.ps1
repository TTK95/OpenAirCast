# Exercises real build.ps1 orchestration/copying without compiling Rust or touching dist/.
$ErrorActionPreference = 'Stop'
$taskRepo = Split-Path $PSScriptRoot -Parent
$taskFixture = Join-Path $taskRepo ('target/build-script-tests/' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $taskFixture -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $taskRepo 'build.ps1') -Destination $taskFixture

# Only the expensive Cargo process is substituted. File copying and failure handling
# are performed by the real script. Simulate a configured Windows target, as on this PC.
function Get-Command {
    [CmdletBinding()]
    param([string]$Name)
    if ($Name -eq 'cargo') { return [pscustomobject]@{ Source = 'Invoke-TestCargo' } }
    Microsoft.PowerShell.Core\Get-Command $Name
}
function Invoke-TestCargo {
    $global:openAirCastBuildTest.Calls.Add(@($args))
    if ($args[0] -eq $global:openAirCastBuildTest.FailCommand) { $global:LASTEXITCODE = 1; return }
    $global:LASTEXITCODE = 0
    if ($args[0] -ne 'build') { return }
    $taskOutput = Join-Path $global:openAirCastBuildTest.Fixture 'target/x86_64-pc-windows-msvc/release'
    New-Item -ItemType Directory -Path $taskOutput -Force | Out-Null
    [IO.File]::WriteAllBytes((Join-Path $taskOutput 'openaircast.exe'), [byte[]](1, 2, 3, 4))
}

$taskDist = Join-Path $taskFixture 'dist/OpenAirCast.exe'
foreach ($taskFailCommand in @('test', 'build', 'none')) {
    New-Item -ItemType Directory -Path (Split-Path $taskDist) -Force | Out-Null
    [IO.File]::WriteAllBytes($taskDist, [byte[]](9))
    $global:openAirCastBuildTest = @{
        Calls = [Collections.Generic.List[object]]::new()
        FailCommand = $taskFailCommand
        Fixture = $taskFixture
    }
    $taskFailed = $false
    try { & (Join-Path $taskFixture 'build.ps1') } catch { $taskFailed = $true }
    if ($taskFailCommand -eq 'none') {
        if ($taskFailed) { throw 'Successful targeted Cargo build did not reach dist/OpenAirCast.exe.' }
        if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($taskDist)) -ne 'AQIDBA==') {
            throw 'dist contains stale output instead of the newly built executable.'
        }
        foreach ($taskCall in $global:openAirCastBuildTest.Calls) {
            if ($taskCall -notcontains '--locked' -or $taskCall -notcontains 'x86_64-pc-windows-msvc') {
                throw 'Cargo invocation does not pin the lockfile and Windows target.'
            }
        }
    } else {
        if (-not $taskFailed -or [IO.File]::ReadAllBytes($taskDist)[0] -ne 9) {
            throw 'Failed validation/build overwrote the previous distribution executable.'
        }
        if ($taskFailCommand -eq 'test' -and $global:openAirCastBuildTest.Calls.Count -ne 1) {
            throw 'Build continued after failed validation.'
        }
    }
}
Write-Host 'PASS: target output copied; failed tests/builds preserve dist. No Rust compilation or app launch.'
