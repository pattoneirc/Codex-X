#requires -Version 7.0
param(
    [Parameter(Mandatory = $true)][string]$CargoJson,
    [string]$OutputDirectory = [System.IO.Path]::GetTempPath()
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw 'This helper only runs on the Windows CI runner.'
}
if ($env:GITHUB_ACTIONS -ne 'true' -or -not $env:RUNNER_TEMP) {
    throw 'This helper is restricted to an ephemeral Windows Actions runner.'
}

# Cargo, not a filesystem wildcard or a caller-supplied EXE, selects the target.
$executables = @(Get-Content -LiteralPath $CargoJson | ForEach-Object {
    if (-not [string]::IsNullOrWhiteSpace($_)) {
        $record = ConvertFrom-Json -InputObject $_ -AsHashtable
        if ($record['reason'] -eq 'compiler-artifact' -and
            $record['target']['name'] -eq 'codexx_lib' -and
            $record['profile']['test'] -eq $true -and
            $record['executable'] -is [string]) {
            [System.IO.Path]::GetFullPath($record['executable'])
        }
    }
} | Sort-Object -Unique)
if ($executables.Count -ne 1) {
    throw "Expected one codexx_lib test executable in Cargo JSON; found $($executables.Count)."
}
$testExecutable = (Resolve-Path -LiteralPath $executables[0]).Path
$testFile = Get-Item -LiteralPath $testExecutable
if ($testFile.Name -notmatch '^codexx_lib-[0-9a-f]{8,32}\.exe$' -or
    $testFile.Directory.Name -ne 'deps' -or
    $testFile.Directory.Parent.Name -notin @('release', 'debug')) {
    throw "Refusing to modify a production or non-harness executable: $testExecutable"
}

$sdkRoot = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
$manifestTool = @(Get-ChildItem -LiteralPath $sdkRoot -Directory |
    Where-Object { $_.Name -match '^\d+\.\d+\.\d+\.\d+$' } |
    Sort-Object { [version]$_.Name } -Descending |
    ForEach-Object { Join-Path $_.FullName 'x64\mt.exe' } |
    Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
    Select-Object -First 1)
if ($manifestTool.Count -ne 1) { throw 'Windows SDK x64 mt.exe was not found.' }
$manifestTool = $manifestTool[0]

function Invoke-ManifestTool([string[]]$ToolArguments) {
    $start = [System.Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $manifestTool
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in $ToolArguments) { $start.ArgumentList.Add($argument) }
    $manifestProcess = [System.Diagnostics.Process]::new()
    $manifestProcess.StartInfo = $start
    try {
        if (-not $manifestProcess.Start()) { throw 'Unable to start SDK mt.exe.' }
        $stdout = $manifestProcess.StandardOutput.ReadToEndAsync()
        $stderr = $manifestProcess.StandardError.ReadToEndAsync()
        $manifestProcess.WaitForExit()
        return @{ Code = $manifestProcess.ExitCode; Text = $stdout.Result + $stderr.Result }
    } finally { $manifestProcess.Dispose() }
}
function Require-ManifestTool([string[]]$ToolArguments) {
    $result = Invoke-ManifestTool $ToolArguments
    if ($result.Code -ne 0) { throw "mt.exe failed ($($result.Code)): $($result.Text)" }
}

$reviewDirectory = Join-Path $OutputDirectory ('codexx-test-manifest-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $reviewDirectory | Out-Null
$beforeManifest = Join-Path $reviewDirectory 'before.manifest'
$additionManifest = Join-Path $reviewDirectory 'tauri-common-controls.manifest'
$mergedManifest = Join-Path $reviewDirectory 'merged.manifest'
$extractedManifest = Join-Path $reviewDirectory 'embedded.manifest'
$backupExecutable = Join-Path $reviewDirectory ($testFile.Name + '.before')
Copy-Item -LiteralPath $testExecutable -Destination $backupExecutable

$probe = Invoke-ManifestTool @('-nologo', "-inputresource:$testExecutable;#1", "-out:$beforeManifest")
if ($probe.Code -eq 0) {
    $inputs = @($beforeManifest, $additionManifest)
} elseif ($probe.Text -match '(?i)(The specified resource (type|name|language).*cannot be found|The specified image file.*did not contain a resource section)') {
    $inputs = @($additionManifest)
} else {
    throw "Cannot safely extract the existing harness manifest: $($probe.Text)"
}
[System.IO.File]::WriteAllText((Join-Path $reviewDirectory 'extract-before.log'), $probe.Text)

$hasDefinitionIdentity = $false
if ($probe.Code -eq 0) {
    [xml]$before = Get-Content -LiteralPath $beforeManifest -Raw
    $hasDefinitionIdentity = $null -ne $before.SelectSingleNode("/*[local-name()='assembly']/*[local-name()='assemblyIdentity']")
}

# Same dependency tuple as tauri-build's default windows-app-manifest.xml.
$addition = @'
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*" />
    </dependentAssembly>
  </dependency>
</assembly>
'@
[System.IO.File]::WriteAllText($additionManifest, $addition, [System.Text.UTF8Encoding]::new($false))
if (-not $hasDefinitionIdentity) {
    [xml]$definition = $addition
    $identity = $definition.CreateElement('assemblyIdentity', $definition.DocumentElement.NamespaceURI)
    $identity.SetAttribute('type', 'win32')
    $identity.SetAttribute('name', 'Codex-X.TestHarness')
    $identity.SetAttribute('version', '1.0.0.0')
    $identity.SetAttribute('processorArchitecture', 'amd64')
    [void]$definition.DocumentElement.PrependChild($identity)
    [System.IO.File]::WriteAllText($additionManifest, $definition.OuterXml, [System.Text.UTF8Encoding]::new($false))
}
Require-ManifestTool (@('-nologo', '-manifest') + $inputs + @("-out:$mergedManifest"))
Require-ManifestTool @('-nologo', '-manifest', $mergedManifest, '-validate_manifest')
Require-ManifestTool @('-nologo', '-manifest', $mergedManifest, "-outputresource:$testExecutable;#1")
Require-ManifestTool @('-nologo', "-inputresource:$testExecutable;#1", "-out:$extractedManifest")
[xml]$embedded = Get-Content -LiteralPath $extractedManifest -Raw
$identity = $embedded.SelectSingleNode("//*[local-name()='dependency']/*[local-name()='dependentAssembly']/*[local-name()='assemblyIdentity'][@name='Microsoft.Windows.Common-Controls' and @version='6.0.0.0' and @publicKeyToken='6595b64144ccf1df']")
if ($null -eq $identity) { throw 'Extracted harness manifest lacks the Common Controls v6 dependency.' }
if ($null -eq $embedded.SelectSingleNode("/*[local-name()='assembly']/*[local-name()='assemblyIdentity']")) {
    throw 'Extracted harness manifest lacks its definition identity.'
}

[ordered]@{
    testExecutable = $testExecutable
    manifestTool = $manifestTool
    hadExistingManifest = ($probe.Code -eq 0)
    originalExecutableBackup = $backupExecutable
    extractedManifest = $extractedManifest
    commonControlsV6Verified = $true
    testsExecutedByHelper = $false
} | ConvertTo-Json
