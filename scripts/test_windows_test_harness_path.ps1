# Test the production path guard without executing the manifest helper or any EXE.
#requires -Version 7.0
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw 'This path check requires Windows.'
}

$helper = Join-Path $PSScriptRoot 'ensure-test-common-controls.ps1'
$tokens = $null
$parseErrors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile($helper, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count -ne 0) { throw "Unable to parse manifest helper: $parseErrors" }
$functions = @($ast.FindAll({
    param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -eq 'Test-TestHarnessPath'
}, $true))
if ($functions.Count -ne 1) { throw 'Expected exactly one production test-harness path guard.' }
. ([scriptblock]::Create($functions[0].Extent.Text))

$cases = @(
    @{Path='D:\a\Codex-X\Codex-X\apps\desktop\src-tauri\target\release\deps\codexx_lib-d3dd1616eccbd474.exe';Expected=$true},
    @{Path='C:\fixture\target\debug\deps\codexx_lib-0123456789abcdef.exe';Expected=$true},
    @{Path='D:\a\Codex-X\Codex-X\apps\desktop\src-tauri\target\release\build\codex-x\d3dd1616eccbd474\out\codexx_lib-d3dd1616eccbd474.exe';Expected=$true},
    @{Path='C:\fixture\target\debug\build\codex-x\0123456789abcdef\out\codexx_lib-0123456789abcdef.exe';Expected=$true},
    @{Path='C:\fixture\target\release\codex-x.exe';Expected=$false},
    @{Path='C:\fixture\target\release\codexx_lib-d3dd1616eccbd474.exe';Expected=$false},
    @{Path='C:\fixture\target\release\deps\codex-x.exe';Expected=$false},
    @{Path='C:\fixture\target\release\deps\codexx_lib.exe';Expected=$false},
    @{Path='C:\fixture\target\release\build\codex-x\d3dd1616eccbd474\out\codexx_lib-0123456789abcdef.exe';Expected=$false},
    @{Path='C:\fixture\target\release\build\other-package\d3dd1616eccbd474\out\codexx_lib-d3dd1616eccbd474.exe';Expected=$false},
    @{Path='C:\fixture\target\release\build\codex-x\d3dd1616eccbd474\other\codexx_lib-d3dd1616eccbd474.exe';Expected=$false},
    @{Path='C:\fixture\target\other\build\codex-x\d3dd1616eccbd474\out\codexx_lib-d3dd1616eccbd474.exe';Expected=$false},
    @{Path='C:\fixture\target\release\build\codex-x\1234\out\codexx_lib-1234.exe';Expected=$false},
    @{Path='C:\fixture\target\release\build\codex-x\out\codexx_lib-d3dd1616eccbd474.exe';Expected=$false}
)
foreach ($case in $cases) {
    $actual = Test-TestHarnessPath $case.Path
    if ($actual -ne $case.Expected) {
        throw "Harness path guard returned $actual, expected $($case.Expected): $($case.Path)"
    }
}
Write-Host "Passed: $($cases.Count) Cargo test-harness path cases; no executable was modified or started."
