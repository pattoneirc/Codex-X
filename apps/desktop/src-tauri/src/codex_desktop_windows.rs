//! Windows desktop discovery and activation. This module deliberately uses
//! Windows' registered package/signature/process APIs, never executable names
//! alone, before stopping a process or attaching to its debugging endpoint.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WindowsDesktopApp {
    pub(crate) path: PathBuf,
    pub(crate) executable: PathBuf,
    pub(crate) bundle_id: String,
    pub(crate) name: String,
    pub(crate) version: String,
}

fn stamps() -> &'static Mutex<HashMap<u32, String>> {
    static STAMPS: OnceLock<Mutex<HashMap<u32, String>>> = OnceLock::new();
    STAMPS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn powershell(script: &str, data: Value, timeout: Duration) -> Result<Value, String> {
    let system_root = std::env::var_os("SystemRoot").ok_or("无法定位 Windows 系统目录")?;
    let executable =
        PathBuf::from(system_root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let encoded = base64::engine::general_purpose::STANDARD
        .encode(serde_json::to_vec(&data).map_err(|_| "无法编码 Windows 客户端操作参数")?);
    // All variable input is JSON data. Paths and config values are never
    // interpolated as PowerShell source or passed through a command shell.
    let source = format!(
        "$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; \
         [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); \
         $data=([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{encoded}')) | ConvertFrom-Json);\n\
         {COMMON}\n{script}"
    );
    let output = crate::platform::desktop_command_output(
        &executable,
        &[
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &source,
        ],
        timeout,
    )?;
    if !output.status.success() {
        let details = String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(1800)
            .collect::<String>();
        return Err(format!("Windows Codex 操作未完成：{}", details.trim()));
    }
    serde_json::from_slice(&output.stdout).map_err(|_| "Windows 未返回有效的客户端操作结果".into())
}

const COMMON: &str = r#"
function FullPath($path) { [IO.Path]::GetFullPath([string]$path).TrimEnd('\') }
function SamePath($left, $right) {
  [string]::Equals((FullPath $left), (FullPath $right), [StringComparison]::OrdinalIgnoreCase)
}
function ProcessStamp($process) {
  if ($null -eq $process.CreationDate) { throw 'Cannot verify the process creation time.' }
  $process.CreationDate.ToUniversalTime().Ticks.ToString([Globalization.CultureInfo]::InvariantCulture)
}
function AppProcesses($exe) {
  $processes=@(Get-CimInstance Win32_Process -Filter "Name='Codex.exe' OR Name='ChatGPT.exe'")
  if (@($processes | Where-Object { -not $_.ExecutablePath }).Count -gt 0) {
    throw 'A Codex/ChatGPT process path is inaccessible. Use matching non-elevated permissions before restarting.'
  }
  @($processes | Where-Object { SamePath $_.ExecutablePath $exe })
}
function MainProcesses($exe) {
  @(AppProcesses $exe | Where-Object {
    if ($null -eq $_.CommandLine) { throw 'Cannot verify the desktop process command line.' }
    $_.CommandLine -notmatch '(?i)(?:^|\s)--type(?:=|\s)' -and
      $_.CommandLine -notmatch '(?i)(?:^|\s)--crashpad-handler(?:\s|$)'
  })
}
function BoundProcess($entry, $exe) {
  $process=Get-Process -Id $entry.ProcessId -ErrorAction SilentlyContinue
  if ($null -eq $process) { return $null }
  try {
    # Holding this handle pins the process object/PID through the subsequent
    # identity checks and CloseMainWindow/Kill call.
    $null=$process.Handle
    $created=$process.StartTime.ToUniversalTime().Ticks
    $expected=[long](ProcessStamp $entry)
    if (-not (SamePath $process.MainModule.FileName $exe) -or
        [Math]::Abs($created-$expected) -ge 10) { $process.Dispose(); return $null }
    # CIM timestamps have microsecond precision, while StartTime has 100ns.
    return $process
  } catch { $process.Dispose(); return $null }
}
function IsDesktopExe($exe) {
  $leaf=[IO.Path]::GetFileName([string]$exe)
  ($leaf -ieq 'Codex.exe' -or $leaf -ieq 'ChatGPT.exe') -and
    (Test-Path -LiteralPath $exe -PathType Leaf) -and
    (Test-Path -LiteralPath (Join-Path ([IO.Path]::GetDirectoryName($exe)) 'resources\app.asar') -PathType Leaf)
}
function TrustedStandalone($exe) {
  if (-not (IsDesktopExe $exe)) { return $false }
  $signature=Get-AuthenticodeSignature -LiteralPath $exe
  if ($signature.Status -ne 'Valid' -or $null -eq $signature.SignerCertificate) { return $false }
  $publisher=$signature.SignerCertificate.GetNameInfo([Security.Cryptography.X509Certificates.X509NameType]::SimpleName,$false)
  @('OpenAI OpCo, LLC','OpenAI, L.L.C.') -ccontains $publisher
}
function TrustedPackage($package) {
  $null -ne $package -and
    @('OpenAI.Codex','OpenAI.CodexBeta','OpenAI.ChatGPT-Desktop') -ccontains [string]$package.Name -and
    [string]$package.PublisherId -ceq '2p2nqsd0c76g0' -and
    [string]$package.PackageFamilyName -ceq ($package.Name+'_2p2nqsd0c76g0') -and
    [string]$package.Status -eq 'Ok' -and [string]$package.SignatureKind -eq 'Store' -and
    -not $package.IsDevelopmentMode
}
function PackageApp($package) {
  $root=FullPath $package.InstallLocation
  $manifest=Get-AppxPackageManifest -Package $package.PackageFullName
  foreach ($entry in @($manifest.Package.Applications.Application)) {
    if (-not $entry.Executable -or -not $entry.Id) { continue }
    $exe=FullPath (Join-Path $root ([string]$entry.Executable))
    if (-not $exe.StartsWith($root+'\',[StringComparison]::OrdinalIgnoreCase)) { continue }
    if (-not (IsDesktopExe $exe)) { continue }
    return [pscustomobject]@{path=$root;executable=$exe;bundleId=($package.PackageFamilyName+'!'+$entry.Id);
      name=$(if($package.Name -eq 'OpenAI.ChatGPT-Desktop'){'ChatGPT'}else{'Codex'});version=[string]$package.Version}
  }
  return $null
}
function ValidateApp($app) {
  if (-not (IsDesktopExe $app.executable)) { throw 'The selected path is not the official Electron desktop application.' }
  if ([string]$app.bundleId -eq 'OpenAI.Codex.Standalone') {
    if (-not (SamePath ([IO.Path]::GetDirectoryName($app.executable)) $app.path) -or
        -not (TrustedStandalone $app.executable)) { throw 'The desktop executable has no valid OpenAI publisher signature.' }
    return
  }
  $principal=[Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
  if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Microsoft Store activation requires Codex-X to run without administrator elevation. No client was stopped.'
  }
  $family=([string]$app.bundleId).Split('!')[0]
  $package=@(Get-AppxPackage | Where-Object { [string]$_.PackageFamilyName -ceq $family }) | Select-Object -First 1
  if (-not (TrustedPackage $package)) { throw 'The official signed Microsoft Store package is no longer registered.' }
  $registered=PackageApp $package
  if ($null -eq $registered -or $registered.bundleId -cne $app.bundleId -or
      -not (SamePath $registered.path $app.path) -or
      -not (SamePath $registered.executable $app.executable)) { throw 'The registered Store application identity has changed.' }
}
function Emit($result) { ConvertTo-Json -InputObject $result -Depth 6 -Compress }
"#;

pub(crate) fn detect() -> Result<WindowsDesktopApp, String> {
    type Cached = (Instant, Result<WindowsDesktopApp, String>);
    static CACHE: OnceLock<Mutex<Option<Cached>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "Windows 客户端检测状态忙")?;
    if let Some((recorded, value)) = cache.as_ref() {
        let ttl = if value.is_ok() { 30 } else { 10 };
        if recorded.elapsed() < Duration::from_secs(ttl) {
            return value.clone();
        }
    }
    let value = detect_fresh();
    *cache = Some((Instant::now(), value.clone()));
    value
}

pub(crate) fn detect_fresh() -> Result<WindowsDesktopApp, String> {
    let result = powershell(
        r#"
$candidates=New-Object 'System.Collections.Generic.List[object]'
foreach ($package in @(Get-AppxPackage | Sort-Object -Property Version -Descending)) {
  if (-not (TrustedPackage $package)) { continue }
  try { $app=PackageApp $package } catch { continue }
  if ($null -ne $app) { $candidates.Add($app) }
}
if ($env:LOCALAPPDATA) {
  $roots=@('OpenAI\Codex\bin','OpenAI\Codex','Programs\OpenAI\Codex','Programs\Codex')
  $seen=@{}
  foreach ($relative in $roots) {
    $root=Join-Path $env:LOCALAPPDATA $relative
    $directories=@($root,(Join-Path $root 'app'))
    if (Test-Path -LiteralPath $root -PathType Container) {
      foreach ($child in @(Get-ChildItem -LiteralPath $root -Directory -ErrorAction SilentlyContinue | Select-Object -First 32)) {
        $directories+=@($child.FullName,(Join-Path $child.FullName 'app'))
      }
    }
    foreach ($directory in $directories) {
      foreach ($leaf in @('Codex.exe','ChatGPT.exe')) {
        $exe=Join-Path $directory $leaf
        $key=$exe.ToLowerInvariant()
        if ($seen.ContainsKey($key)) { continue }; $seen[$key]=$true
        if (-not (TrustedStandalone $exe)) { continue }
        $version=(Get-Item -LiteralPath $exe).VersionInfo.ProductVersion
        $candidates.Add([pscustomobject]@{path=(FullPath $directory);executable=(FullPath $exe);
          bundleId='OpenAI.Codex.Standalone';name='Codex';version=[string]$version})
      }
    }
  }
}
$running=@()
foreach ($app in $candidates) {
  $main=@(MainProcesses $app.executable)
  if ($main.Count -gt 1) { throw 'Multiple desktop instances are running. Keep one instance before restarting.' }
  if ($main.Count -eq 1) { $running+=,$app }
}
if ($running.Count -gt 1) { throw 'Multiple official desktop installations are running. Keep one before restarting.' }
if ($running.Count -eq 1) { Emit $running[0]; exit }
if ($candidates.Count -eq 0) { throw 'No verified OpenAI Codex Store package or signed standalone installation was found.' }
Emit $candidates[0]
"#,
        Value::Null,
        Duration::from_secs(25),
    )?;
    serde_json::from_value(result).map_err(|_| "Windows 客户端安装信息无效".into())
}

pub(crate) fn validate(app: &WindowsDesktopApp) -> Result<(), String> {
    powershell(
        "ValidateApp $data.app; Emit $true",
        json!({"app":app}),
        Duration::from_secs(15),
    )
    .map(|_| ())
}

pub(crate) fn validate_launch(app: &WindowsDesktopApp, home: &Path) -> Result<(), String> {
    if app.bundle_id != "OpenAI.Codex.Standalone" {
        let default = dirs::home_dir()
            .ok_or("无法确认 Windows 默认 Codex 配置目录")?
            .join(".codex");
        let requested = crate::paths::normalized_path_scope(home);
        let expected = crate::paths::normalized_path_scope(&default);
        if requested != expected {
            return Err("Windows Store 版无法通过安全激活传入自定义 CODEX_HOME；请使用默认 .codex 目录或官方独立安装版。客户端尚未退出。".into());
        }
    }
    Ok(())
}

pub(crate) fn pids(app: &WindowsDesktopApp) -> Result<Vec<u32>, String> {
    let value = powershell(
        "Emit @(AppProcesses $data.app.executable | ForEach-Object { [uint32]$_.ProcessId })",
        json!({"app":app}),
        Duration::from_secs(8),
    )?;
    serde_json::from_value(value).map_err(|_| "无法确认 Windows 客户端进程".into())
}

pub(crate) fn quit(app: &WindowsDesktopApp) -> Result<(), String> {
    powershell(
        r#"
ValidateApp $data.app
$main=@(MainProcesses $data.app.executable)
if ($main.Count -gt 1) { throw 'Multiple desktop instances are running; none was stopped.' }
foreach ($entry in $main) {
  $process=BoundProcess $entry $data.app.executable
  if ($null -ne $process) {
    try { $null=$process.CloseMainWindow() } finally { $process.Dispose() }
  }
}
$grace=[DateTime]::UtcNow.AddSeconds(2)
while ([DateTime]::UtcNow -lt $grace -and @(AppProcesses $data.app.executable).Count -gt 0) { Start-Sleep -Milliseconds 100 }
# The user explicitly requested a restart. If a tray/renderer remains, stop
# only processes whose live executable path still matches this verified app.
foreach ($entry in @(AppProcesses $data.app.executable)) {
  $process=BoundProcess $entry $data.app.executable
  if ($null -ne $process) {
    try { $process.Kill() } catch { } finally { $process.Dispose() }
  }
}
$deadline=[DateTime]::UtcNow.AddSeconds(8); $emptySince=$null
do {
  if (@(AppProcesses $data.app.executable).Count -eq 0) {
    if ($null -eq $emptySince) { $emptySince=[DateTime]::UtcNow }
    if (([DateTime]::UtcNow-$emptySince).TotalMilliseconds -ge 500) { Emit $true; exit }
  } else { $emptySince=$null }
  Start-Sleep -Milliseconds 100
} while ([DateTime]::UtcNow -lt $deadline)
throw 'The selected desktop processes did not stop; relaunch was cancelled.'
"#,
        json!({"app":app}),
        Duration::from_secs(20),
    )
    .map(|_| ())
}

pub(crate) fn launch(
    app: &WindowsDesktopApp,
    home: Option<&Path>,
    port: Option<u16>,
) -> Result<u32, String> {
    if let Some(home) = home {
        validate_launch(app, home)?;
    }
    if app.bundle_id == "OpenAI.Codex.Standalone" {
        return launch_standalone(app, home, port);
    }
    let result = powershell(
        r#"
ValidateApp $data.app
if (@(AppProcesses $data.app.executable).Count -gt 0) { throw 'The desktop application is still stopping; no duplicate instance was launched.' }
$arguments=''
if ($null -ne $data.port) { $arguments='--remote-debugging-port='+[string]$data.port+' --remote-debugging-address=127.0.0.1' }
  Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
[ComImport, Guid("2e941141-7f97-4756-ba1d-9decde894a3d"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface ICodexActivationManager {
  [PreserveSig] int ActivateApplication([MarshalAs(UnmanagedType.LPWStr)] string appId,
    [MarshalAs(UnmanagedType.LPWStr)] string arguments, uint options, out uint processId);
}
public static class CodexPackageActivation {
  public static uint Start(string appId, string arguments) {
    var manager=(ICodexActivationManager)Activator.CreateInstance(Type.GetTypeFromCLSID(new Guid("45ba127d-10a8-46ea-8ab7-56ea9078943c")));
    try { uint processId; int result=manager.ActivateApplication(appId, arguments, 0, out processId);
      Marshal.ThrowExceptionForHR(result); return processId; }
    finally { Marshal.FinalReleaseComObject(manager); }
  }
}
'@ | Out-Null
  $started=[CodexPackageActivation]::Start([string]$data.app.bundleId,$arguments)
if ($started -eq 0) { throw 'Windows did not return an activated process id.' }
$created=''; $deadline=[DateTime]::UtcNow.AddSeconds(5)
do {
  $process=Get-CimInstance Win32_Process -Filter ('ProcessId='+$started)
  if ($null -ne $process -and $process.ExecutablePath) {
    if (-not (SamePath $process.ExecutablePath $data.app.executable)) { throw 'Windows activated a different executable; no debugging connection was made.' }
    $created=ProcessStamp $process; break
  }
  Start-Sleep -Milliseconds 100
} while ([DateTime]::UtcNow -lt $deadline)
if (-not $created) { throw 'Windows activation returned but the official desktop process did not start.' }
Emit @{pid=$started;created=$created}
"#,
        json!({"app":app,"home":home,"port":port}),
        Duration::from_secs(20),
    )?;
    remember_started(result)
}

fn remember_started(result: Value) -> Result<u32, String> {
    let pid = result["pid"]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .filter(|pid| *pid != 0)
        .ok_or("Windows 未确认已启动的客户端进程")?;
    let created = result["created"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or("Windows 未确认客户端进程身份")?;
    let mut tracked = stamps().lock().map_err(|_| "Windows 进程状态忙")?;
    if tracked.len() >= 16 {
        tracked.clear();
    }
    tracked.insert(pid, created.to_owned());
    Ok(pid)
}

fn launch_standalone(
    app: &WindowsDesktopApp,
    home: Option<&Path>,
    port: Option<u16>,
) -> Result<u32, String> {
    powershell(
        "ValidateApp $data.app; if (@(AppProcesses $data.app.executable).Count -gt 0) { throw 'The desktop is still stopping; no duplicate was launched.' }; Emit $true",
        json!({"app":app}),
        Duration::from_secs(15),
    )?;
    let arguments = port
        .map(|port| {
            vec![
                format!("--remote-debugging-port={port}"),
                "--remote-debugging-address=127.0.0.1".to_owned(),
            ]
        })
        .unwrap_or_default();
    let args: Vec<_> = arguments.iter().map(String::as_str).collect();
    let mut command = crate::platform::program_command(&app.executable, &args);
    command
        .current_dir(&app.path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(home) = home {
        command.env("CODEX_HOME", home);
    }
    let mut child = command
        .spawn()
        .map_err(|_| "无法启动已验证的 Windows Codex 客户端")?;
    let pid = child.id();
    let result = powershell(
        r#"
$deadline=[DateTime]::UtcNow.AddSeconds(5)
do {
  $process=Get-CimInstance Win32_Process -Filter ('ProcessId='+[uint32]$data.pid)
  if ($null -ne $process -and $process.ExecutablePath) {
    if (-not (SamePath $process.ExecutablePath $data.app.executable)) { throw 'The newly started process has an unexpected executable.' }
    Emit @{pid=[uint32]$data.pid;created=(ProcessStamp $process)}; exit
  }
  Start-Sleep -Milliseconds 100
} while ([DateTime]::UtcNow -lt $deadline)
throw 'The signed desktop executable did not remain running.'
"#,
        json!({"app":app,"pid":pid}),
        Duration::from_secs(10),
    ).and_then(remember_started);
    if result.is_err() {
        // Child owns the original Windows process handle, so cleanup cannot
        // terminate an unrelated process after a recycled PID.
        let _ = child.kill();
        let _ = child.try_wait();
    }
    // Windows has no zombie-process wait requirement. Dropping Child releases
    // our process/thread handles while the GUI keeps its own lifetime.
    drop(child);
    result
}

pub(crate) fn verify_process(app: &WindowsDesktopApp, pid: u32) -> Result<(), String> {
    let created = stamps()
        .lock()
        .map_err(|_| "Windows 进程状态忙")?
        .get(&pid)
        .cloned()
        .ok_or("该客户端进程不属于本次受控启动")?;
    powershell(
        r#"
$process=Get-CimInstance Win32_Process -Filter ('ProcessId='+[uint32]$data.pid)
if ($null -eq $process -or -not $process.ExecutablePath -or
    -not (SamePath $process.ExecutablePath $data.app.executable) -or
    (ProcessStamp $process) -cne [string]$data.created) { throw 'The activated desktop process identity has changed.' }
Emit $true
"#,
        json!({"app":app,"pid":pid,"created":created}),
        Duration::from_secs(8),
    )
    .map(|_| ())
}

pub(crate) fn verify_listener(app: &WindowsDesktopApp, pid: u32, port: u16) -> Result<(), String> {
    let created = stamps()
        .lock()
        .map_err(|_| "Windows 进程状态忙")?
        .get(&pid)
        .cloned()
        .ok_or("该客户端进程不属于本次受控启动")?;
    powershell(
        r#"
$process=Get-CimInstance Win32_Process -Filter ('ProcessId='+[uint32]$data.pid)
if ($null -eq $process -or -not $process.ExecutablePath -or
    -not (SamePath $process.ExecutablePath $data.app.executable) -or
    (ProcessStamp $process) -cne [string]$data.created) { throw 'The activated desktop process identity has changed.' }
$listeners=@(Get-NetTCPConnection -State Listen -LocalPort ([uint16]$data.port) -ErrorAction SilentlyContinue)
if ($listeners.Count -eq 0) { throw 'The official desktop has not opened its debugging port.' }
foreach ($listener in $listeners) {
  if ($listener.LocalAddress -cne '127.0.0.1' -or [uint32]$listener.OwningProcess -ne [uint32]$data.pid) {
    throw 'The debugging port is not exclusively owned by the activated process on 127.0.0.1.'
  }
}
$after=Get-CimInstance Win32_Process -Filter ('ProcessId='+[uint32]$data.pid)
if ($null -eq $after -or -not $after.ExecutablePath -or
    -not (SamePath $after.ExecutablePath $data.app.executable) -or
    (ProcessStamp $after) -cne [string]$data.created) { throw 'The desktop process changed while its port was being verified.' }
Emit $true
"#,
        json!({"app":app,"pid":pid,"port":port,"created":created}),
        Duration::from_secs(8),
    )
    .map(|_| ())
}

pub(crate) fn restart() -> Result<(String, bool), String> {
    let app = detect_fresh()?;
    validate(&app)?;
    let was_running = !pids(&app)?.is_empty();
    quit(&app)?;
    launch(&app, None, None)?;
    Ok((app.name, was_running))
}
