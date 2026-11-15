# Execute the production PowerShell helpers against isolated fixture processes.
# No official Codex/ChatGPT process is inspected or stopped by these fixtures.
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'This check requires Windows.' }
$source = Get-Content (Join-Path $PSScriptRoot '../apps/desktop/src-tauri/src/codex_desktop_windows.rs') -Raw -Encoding UTF8
function Constant([string]$Name) {
  $match = [regex]::Match($source, '(?s)const ' + [regex]::Escape($Name) + ': &str = r#"(.*?)"#;')
  if (-not $match.Success) { throw "Missing production/test constant: $Name" }
  $match.Groups[1].Value
}
$common = Constant 'COMMON'
$stop = Constant 'STOP_DESKTOP'
$fixtures = Constant 'PROCESS_FIXTURES'
$cases = @(
  @{Name='cim_missing_path_uses_live_identity_and_ignores_exited_foreign_and_recycled_pids';Expected=@{ids=@(101);probed=@(101,102,105,106)}},
  @{Name='inaccessible_live_process_aborts_before_any_client_is_closed';Expected=@{closed=$false;error='CODEXX_PROCESS_ACCESS_REQUIRED: PID 101'}},
  @{Name='stop_waits_on_bound_handles_instead_of_pathless_cim_rows_after_close';Expected=@{closed=$true;queries=1;disposed=1}},
  @{Name='stop_binds_all_processes_before_closing_and_releases_handles_on_preflight_failure';Expected=@{closed=$false;disposed=1;error='fixture: selected helper cannot be controlled'}}
)
foreach ($case in $cases) {
  $start=$source.IndexOf('fn '+$case.Name+'(')
  if ($start -lt 0) { throw "Missing fixture: $($case.Name)" }
  $match=[regex]::Match($source.Substring($start), '(?s)process_fixture\(\s*r#"(.*?)"#,\s*(true|false)\s*,?\s*\)')
  if (-not $match.Success) { throw "Cannot extract fixture: $($case.Name)" }
  $result = & {
    $data=[pscustomobject]@{app=[pscustomobject]@{executable='C:\fixture\Codex.exe'}}
    . ([scriptblock]::Create($common))
    . ([scriptblock]::Create($stop))
    . ([scriptblock]::Create($fixtures))
    . ([scriptblock]::Create($match.Groups[1].Value))
  }
  $actual = ($result -join "`n") | ConvertFrom-Json
  foreach ($key in $case.Expected.Keys) {
    $left=ConvertTo-Json -InputObject $actual.$key -Compress
    $right=ConvertTo-Json -InputObject $case.Expected[$key] -Compress
    if ($left -cne $right) { throw "$($case.Name): $key returned $left, expected $right" }
  }
  Write-Host "Passed: $($case.Name)"
}
# Exercise the actual elevated Store flow with mocked activation only. These
# fixtures never contact Explorer or launch/stop a desktop application.
$storeStart = Constant 'START_WITH_DESKTOP_SHELL'
$storeRestart = Constant 'ELEVATED_STORE_RESTART'
$storeCases = @(
  @{Name='elevated_store_restart_keeps_client_running_when_broker_preflight_fails';Expected=@{handled=$true;ok=$false;afterAttempt=$false;error='CODEXX_STORE_BROKER_UNAVAILABLE: fixture: no same-user unelevated shell'}},
  @{Name='elevated_store_restart_activates_verified_identity_after_preflight_and_stop';Expected=@{handled=$true;ok=$true;wasRunning=$true;pid=4242}},
  @{Name='elevated_store_activation_failure_reports_that_close_already_started';Expected=@{handled=$true;ok=$false;afterAttempt=$true;error='fixture activation failed after stop'}},
  @{Name='elevated_store_start_failure_without_an_old_process_still_requires_observation';Expected=@{handled=$true;ok=$false;afterAttempt=$true;error='fixture activation confirmation was delayed'}},
  @{Name='unelevated_store_restart_uses_existing_activation_path';Expected=@{handled=$false}}
)
foreach ($case in $storeCases) {
  $start=$source.IndexOf('fn '+$case.Name+'(')
  if ($start -lt 0) { throw "Missing Store fixture: $($case.Name)" }
  $match=[regex]::Match($source.Substring($start), '(?s)restart_fixture\(\s*r#"(.*?)"#,\s*(true|false)\s*,?\s*\)')
  if (-not $match.Success) { throw "Cannot extract Store fixture: $($case.Name)" }
  $result = & {
    $data=[pscustomobject]@{app=[pscustomobject]@{executable='fixture.exe';bundleId='OpenAI.Codex_2p2nqsd0c76g0!App'}}
    . ([scriptblock]::Create($common))
    . ([scriptblock]::Create($stop))
    . ([scriptblock]::Create($storeStart))
    . ([scriptblock]::Create($match.Groups[1].Value))
    . ([scriptblock]::Create($storeRestart))
  }
  $actual = ($result -join "`n") | ConvertFrom-Json
  foreach ($key in $case.Expected.Keys) {
    $left=ConvertTo-Json -InputObject $actual.$key -Compress
    $right=ConvertTo-Json -InputObject $case.Expected[$key] -Compress
    if ($left -cne $right) { throw "$($case.Name): $key returned $left, expected $right" }
  }
  Write-Host "Passed: $($case.Name)"
}
# Exercise the same ordinary Store activation helpers used by restart and CDP.
# Activation is mocked; the reported launcher PID deliberately differs from
# the desktop PID. Enumeration and main-process selection remain production code.
$activation = Constant 'STORE_ACTIVATION'
$activationFixtures = Constant 'ACTIVATION_FIXTURES'
$activationCases = @(
  @{Name='store_activation_tracks_desktop_main_instead_of_launcher_pid';Expected=@{pid=4242;created=$true;calls=1;queries=3}},
  @{Name='store_activation_rejects_unrelated_executable';Expected=@{rejected=$true;calls=1}},
  @{Name='store_activation_does_not_duplicate_an_existing_main';Expected=@{pid=4242;calls=0}},
  @{Name='store_activation_preserves_debug_arguments';Expected=@{pid=4242;calls=1;arguments='--remote-debugging-port=49152 --remote-debugging-address=127.0.0.1'}}
)
foreach ($case in $activationCases) {
  $start=$source.IndexOf('fn '+$case.Name+'(')
  if ($start -lt 0) { throw "Missing activation fixture: $($case.Name)" }
  $match=[regex]::Match($source.Substring($start), '(?s)activation_fixture\(\s*r#"(.*?)"#\s*,?\s*\)')
  if (-not $match.Success) { throw "Cannot extract activation fixture: $($case.Name)" }
  $result = & {
    $data=[pscustomobject]@{app=[pscustomobject]@{executable='C:\fixture\Codex.exe';bundleId='OpenAI.Codex_2p2nqsd0c76g0!App'}}
    . ([scriptblock]::Create($common))
    . ([scriptblock]::Create($activation))
    . ([scriptblock]::Create($fixtures))
    . ([scriptblock]::Create($activationFixtures))
    . ([scriptblock]::Create($match.Groups[1].Value))
  }
  $actual = ($result -join "`n") | ConvertFrom-Json
  foreach ($key in $case.Expected.Keys) {
    $left=ConvertTo-Json -InputObject $actual.$key -Compress
    $right=ConvertTo-Json -InputObject $case.Expected[$key] -Compress
    if ($left -cne $right) { throw "$($case.Name): $key returned $left, expected $right" }
  }
  Write-Host "Passed: $($case.Name)"
}
# Check exact serialized absence on PowerShell 5.1, and real junction identity
# through both path matching and native binding to this test host's own PID.
$identityCases = @(
  @{Name='NULL_RESULT_FIXTURE';Expected=@{emptyMain='null';explicit='null';emptyArray='[]';falseValue='false'}},
  @{Name='PATH_IDENTITY_FIXTURE';Expected=@{same=$true;different=$false;directory=$true;bound=$true}}
)
foreach ($case in $identityCases) {
  $script=Constant $case.Name
  $result = & {
    $data=[pscustomobject]@{app=[pscustomobject]@{executable='C:\fixture\Codex.exe'}}
    . ([scriptblock]::Create($common))
    . ([scriptblock]::Create($script))
  }
  $actual = ($result -join "`n") | ConvertFrom-Json
  foreach ($key in $case.Expected.Keys) {
    $left=ConvertTo-Json -InputObject $actual.$key -Compress
    $right=ConvertTo-Json -InputObject $case.Expected[$key] -Compress
    if ($left -cne $right) { throw "$($case.Name): $key returned $left, expected $right" }
  }
  Write-Host "Passed: $($case.Name)"
}
# Exercise actual limited-query Windows APIs only against this test host itself.
. ([scriptblock]::Create($common))
EnsureProcessApi
$live=InspectDesktopProcess $PID
try {
  $bound=[CodexDesktopProcess]::Bind([uint32]$PID,$live.ExecutablePath,$live.CreationDate.Ticks,$false)
  try {
    if ($null -eq $bound -or $bound.HasExited) { throw 'Read-only native identity binding failed.' }
    if ($null -ne [CodexDesktopProcess]::Bind([uint32]$PID,($live.ExecutablePath+'.wrong'),$live.CreationDate.Ticks,$false)) { throw 'Path mismatch was accepted.' }
    if ($null -ne [CodexDesktopProcess]::Bind([uint32]$PID,$live.ExecutablePath,($live.CreationDate.Ticks+10000),$false)) { throw 'Creation time mismatch was accepted.' }
  } finally { if ($null -ne $bound) { $bound.Dispose() } }
} finally { $live.Dispose() }
Write-Host 'Passed: native query-only process binding; no application was terminated.'
