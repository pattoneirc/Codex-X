# Read-only loader diagnostics for an ephemeral Windows Actions runner.
# Reads the test PE import/export tables; never invokes imported functions or
# DLL DllMain. Optional --list starts only the Rust test harness, no test bodies.
param(
  [string]$TestExe,
  [string]$OutputDirectory,
  [switch]$RunHarnessList
)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or -not $env:RUNNER_TEMP) {
  throw 'This diagnostic is restricted to an ephemeral Windows Actions runner.'
}
if (-not $TestExe) {
  $candidate = Get-ChildItem 'apps/desktop/src-tauri/target/release/deps/codexx_lib-*.exe' |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
  if (-not $candidate) { throw 'Compile cargo test --release --lib --no-run first.' }
  $TestExe = $candidate.FullName
}
$TestExe = (Resolve-Path -LiteralPath $TestExe).Path
if ((Split-Path -Leaf $TestExe) -notlike 'codexx_lib-*.exe') {
  throw 'Only the Codex-X Rust test harness may be inspected.'
}
if (-not $OutputDirectory) {
  $OutputDirectory = Join-Path $env:RUNNER_TEMP 'codexx-dll-diagnostics'
}
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$ExeDirectory = Split-Path -Parent $TestExe
$dumpbinCommand = Get-Command dumpbin.exe -ErrorAction SilentlyContinue
if ($dumpbinCommand) { $Dumpbin = $dumpbinCommand.Source }
else {
  $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
  if (-not (Test-Path -LiteralPath $vswhere)) { throw 'Neither dumpbin nor vswhere was found.' }
  $vs = (& $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath | Select-Object -First 1)
  $Dumpbin = (Get-ChildItem "$vs/VC/Tools/MSVC/*/bin/Hostx64/x64/dumpbin.exe" |
    Sort-Object FullName -Descending | Select-Object -First 1).FullName
  if (-not $Dumpbin) { throw 'MSVC dumpbin.exe was not found.' }
}
$report = [ordered]@{
  TestExe = $TestExe
  TestSha256 = (Get-FileHash -LiteralPath $TestExe -Algorithm SHA256).Hash
  ImageOS = $env:ImageOS
  ImageVersion = $env:ImageVersion
  OS = [Environment]::OSVersion.VersionString
  Dumpbin = $Dumpbin
  Rust = ((& rustc --version --verbose) -join "`n")
  Cargo = ((& cargo --version) -join "`n")
  WorkingDirectory = (Get-Location).Path
  Path = $env:PATH
  Modules = @()
  Findings = @()
  Notes = @('DONT_RESOLVE_DLL_REFERENCES prevents DllMain and dependent imports from executing.',
    'PowerShell API-set resolution is recorded with the real module path.',
    'Candidates include test executable directory, System32, Windows and PATH; Cargo OUT_DIRs are appended for diagnostics.',
    'Missing named exports are verified with PE export tables and GetProcAddress; forwarders are followed.',
    'A finding is evidence to compare with the actual test process DLL search/activation context, not a guessed root cause.')
}
& $Dumpbin /imports $TestExe 2>&1 | Out-File (Join-Path $OutputDirectory 'test-imports.txt') -Encoding utf8
& $Dumpbin /dependents $TestExe 2>&1 | Out-File (Join-Path $OutputDirectory 'test-dependents.txt') -Encoding utf8
& $Dumpbin /headers $TestExe 2>&1 | Out-File (Join-Path $OutputDirectory 'test-headers.txt') -Encoding utf8

Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public sealed class CodexxPeImport {
  public string Dll; public string Name; public ushort Ordinal; public bool ByOrdinal;
}
public sealed class CodexxPeExport { public bool Exists; public string Forwarder; }
public static class CodexxLoaderProbe {
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
  public static extern IntPtr LoadLibraryExW(string name, IntPtr file, uint flags);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
  public static extern uint GetModuleFileNameW(IntPtr module, StringBuilder path, int size);
  [DllImport("kernel32.dll", CharSet=CharSet.Ansi, ExactSpelling=true, SetLastError=true)]
  public static extern IntPtr GetProcAddress(IntPtr module, string name);
  [DllImport("kernel32.dll", EntryPoint="GetProcAddress", ExactSpelling=true, SetLastError=true)]
  public static extern IntPtr GetProcAddressOrdinal(IntPtr module, IntPtr ordinal);
  [DllImport("kernel32.dll")] public static extern bool FreeLibrary(IntPtr module);
  [DllImport("kernel32.dll")] public static extern uint SetErrorMode(uint mode);
  static Dictionary<string,byte[]> Files = new Dictionary<string,byte[]>(StringComparer.OrdinalIgnoreCase);
  static Dictionary<string,Dictionary<string,uint>> ExportNames = new Dictionary<string,Dictionary<string,uint>>(StringComparer.OrdinalIgnoreCase);
  static byte[] Bytes(string path) {
    byte[] value; if(Files.TryGetValue(path,out value)) return value;
    value=File.ReadAllBytes(path); Files[path]=value; return value;
  }
  static ushort U16(byte[] b,int p) { return BitConverter.ToUInt16(b,p); }
  static uint U32(byte[] b,int p) { return BitConverter.ToUInt32(b,p); }
  static ulong U64(byte[] b,int p) { return BitConverter.ToUInt64(b,p); }
  static int Optional(byte[] b) {
    if(b.Length<64 || b[0]!=0x4d || b[1]!=0x5a) throw new InvalidDataException("Not PE");
    int pe=checked((int)U32(b,60));
    if(pe+24>b.Length || U32(b,pe)!=0x4550) throw new InvalidDataException("Bad PE header");
    return pe+24;
  }
  static int Directory(byte[] b,int index) {
    int opt=Optional(b); ushort magic=U16(b,opt);
    if(magic!=0x20b && magic!=0x10b) throw new InvalidDataException("Unsupported PE");
    return opt+(magic==0x20b?112:96)+index*8;
  }
  static int Offset(byte[] b,uint rva) {
    int opt=Optional(b); int pe=opt-24;
    uint headerSize=U32(b,opt+60);
    if(rva<headerSize && rva<b.Length) return (int)rva;
    int section=opt+U16(b,pe+20), count=U16(b,pe+6);
    for(int n=0;n<count;n++,section+=40) {
      uint size=Math.Max(U32(b,section+8),U32(b,section+16));
      uint va=U32(b,section+12), raw=U32(b,section+20);
      if(rva>=va && (ulong)rva<(ulong)va+size) {
        ulong p=(ulong)raw+rva-va;
        if(p>=(ulong)b.Length) throw new InvalidDataException("RVA outside file");
        return checked((int)p);
      }
    }
    throw new InvalidDataException("Unmapped RVA");
  }
  static string Z(byte[] b,int p) {
    int end=p; while(end<b.Length && b[end]!=0 && end-p<4096) end++;
    if(end==b.Length || end-p==4096) throw new InvalidDataException("Unterminated PE string");
    return Encoding.ASCII.GetString(b,p,end-p);
  }
  public static CodexxPeImport[] Imports(string path) {
    byte[] b=Bytes(path); int dir=Directory(b,1); uint rva=U32(b,dir);
    var result=new List<CodexxPeImport>(); if(rva==0) return result.ToArray();
    int desc=Offset(b,rva); bool x64=U16(b,Optional(b))==0x20b;
    for(int d=0;d<4096;d++,desc+=20) {
      uint nameRva=U32(b,desc+12), thunkRva=U32(b,desc);
      if(nameRva==0) break; if(thunkRva==0) thunkRva=U32(b,desc+16);
      string dll=Z(b,Offset(b,nameRva)); int thunk=Offset(b,thunkRva);
      for(int n=0;n<100000;n++,thunk+=(x64?8:4)) {
        ulong v=x64?U64(b,thunk):U32(b,thunk); if(v==0) break;
        bool ordinal=(v & (x64?0x8000000000000000UL:0x80000000UL))!=0;
        result.Add(new CodexxPeImport { Dll=dll, ByOrdinal=ordinal,
          Ordinal=(ushort)(v & 65535), Name=ordinal?null:Z(b,Offset(b,checked((uint)v))+2) });
      }
    }
    return result.ToArray();
  }
  public static CodexxPeExport Export(string path,string wanted,ushort ordinal,bool byOrdinal) {
    byte[] b=Bytes(path); int dir=Directory(b,0);
    uint rva=U32(b,dir), size=U32(b,dir+4); var result=new CodexxPeExport();
    if(rva==0) return result; int exp=Offset(b,rva);
    uint first=U32(b,exp+16), count=U32(b,exp+20), names=U32(b,exp+24);
    int funcs=Offset(b,U32(b,exp+28)); uint index=UInt32.MaxValue;
    if(byOrdinal) { if(ordinal<first || ordinal-first>=count) return result; index=ordinal-first; }
    else {
      Dictionary<string,uint> lookup;
      if(!ExportNames.TryGetValue(path,out lookup)) {
        lookup=new Dictionary<string,uint>(StringComparer.Ordinal);
        int nameTable=Offset(b,U32(b,exp+32)), ordTable=Offset(b,U32(b,exp+36));
        for(uint n=0;n<names;n++) lookup[Z(b,Offset(b,U32(b,nameTable+checked((int)n*4))))]=U16(b,ordTable+checked((int)n*2));
        ExportNames[path]=lookup;
      }
      if(!lookup.TryGetValue(wanted,out index)) return result;
    }
    if(index==UInt32.MaxValue || index>=count) return result;
    uint target=U32(b,funcs+checked((int)index*4)); if(target==0) return result;
    result.Exists=true;
    if(target>=rva && (ulong)target<(ulong)rva+size) result.Forwarder=Z(b,Offset(b,target));
    return result;
  }
}
'@

$searchFolders = [Collections.Generic.List[string]]::new()
$searchFolders.Add($ExeDirectory)
$searchFolders.Add((Join-Path $env:SystemRoot 'System32'))
$searchFolders.Add($env:SystemRoot)
$searchFolders.Add((Get-Location).Path)
foreach ($part in ($env:PATH -split ';')) { if ($part) { $searchFolders.Add($part.Trim('"')) } }
# Cargo adds native build output directories while running a harness. Record all
# candidates, but do not claim this broad diagnostic order is Cargo's exact order.
Get-ChildItem 'apps/desktop/src-tauri/target/release/build/*/out' -Directory -ErrorAction SilentlyContinue |
  ForEach-Object { $searchFolders.Add($_.FullName) }
$knownDlls = @{}
$known = Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\KnownDLLs'
foreach ($property in $known.PSObject.Properties) {
  if ($property.Value -is [string] -and $property.Value -like '*.dll') { $knownDlls[$property.Value.ToLowerInvariant()] = $true }
}
$modules = @{}
$findings = [Collections.Generic.List[object]]::new()
function Get-ProbeModule([string]$Dll) {
  $key = $Dll.ToLowerInvariant()
  if ($modules.ContainsKey($key)) { return $modules[$key] }
  $candidates = @()
  foreach ($directory in $searchFolders) {
    $file = Join-Path $directory $Dll
    if (Test-Path -LiteralPath $file -PathType Leaf) { $candidates += $file }
  }
  $requested = $Dll
  if (-not ($key.StartsWith('api-ms-') -or $key.StartsWith('ext-ms-') -or $knownDlls.ContainsKey($key))) {
    if ($candidates.Count) { $requested = $candidates[0] }
  }
  $handle = [CodexxLoaderProbe]::LoadLibraryExW($requested,[IntPtr]::Zero,1)
  $errorCode = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
  $resolved = $null
  if ($handle -ne [IntPtr]::Zero) {
    $buffer = [Text.StringBuilder]::new(4096)
    if ([CodexxLoaderProbe]::GetModuleFileNameW($handle,$buffer,$buffer.Capacity)) { $resolved = $buffer.ToString() }
  }
  $record = [ordered]@{ Requested=$Dll; LoadArgument=$requested; Path=$resolved; Handle=$handle; LoadError=$errorCode; Candidates=$candidates }
  if ($resolved) {
    $record['Version'] = (Get-Item -LiteralPath $resolved).VersionInfo.FileVersion
    $record['Sha256'] = (Get-FileHash -LiteralPath $resolved -Algorithm SHA256).Hash
    & $Dumpbin /exports $resolved 2>&1 | Out-File (Join-Path $OutputDirectory ('exports-' + $Dll + '.txt')) -Encoding utf8
  }
  $modules[$key] = $record
  return $record
}
function Test-ProbeExport([string]$Dll,[string]$Name,[ushort]$Ordinal,[bool]$ByOrdinal,[string[]]$Chain=@()) {
  $symbol = if ($ByOrdinal) { "#$Ordinal" } else { $Name }
  $node = "$Dll!$symbol"
  if ($Chain.Count -ge 20 -or $node -in $Chain) {
    $findings.Add([ordered]@{ Kind='ForwarderCycle'; Import=$node; Chain=$Chain }); return
  }
  $Chain += $node
  $module = Get-ProbeModule $Dll
  if (-not $module.Path) {
    $findings.Add([ordered]@{ Kind='ModuleLoadFailure'; Import=$node; Win32=$module.LoadError; LoadArgument=$module.LoadArgument; Chain=$Chain }); return
  }
  try { $export = [CodexxLoaderProbe]::Export($module.Path,$Name,$Ordinal,$ByOrdinal) }
  catch {
    $findings.Add([ordered]@{ Kind='ExportParseFailure'; Import=$node; Path=$module.Path; Detail=$_.Exception.GetType().FullName; Chain=$Chain }); return
  }
  if (-not $export.Exists) {
    $findings.Add([ordered]@{ Kind='MissingExport'; Import=$node; Path=$module.Path; Chain=$Chain }); return
  }
  if ($export.Forwarder) {
    $split = $export.Forwarder.LastIndexOf('.')
    if ($split -lt 1) { $findings.Add([ordered]@{ Kind='MalformedForwarder'; Import=$node; Path=$module.Path; Forwarder=$export.Forwarder }); return }
    $targetDll = $export.Forwarder.Substring(0,$split)
    if (-not $targetDll.EndsWith('.dll',[StringComparison]::OrdinalIgnoreCase)) { $targetDll += '.dll' }
    $target = $export.Forwarder.Substring($split+1)
    if ($target.StartsWith('#')) { Test-ProbeExport $targetDll $null ([ushort]$target.Substring(1)) $true $Chain }
    else { Test-ProbeExport $targetDll $target 0 $false $Chain }
    return
  }
  $address = if ($ByOrdinal) { [CodexxLoaderProbe]::GetProcAddressOrdinal($module.Handle,[IntPtr]::new($Ordinal)) }
    else { [CodexxLoaderProbe]::GetProcAddress($module.Handle,$Name) }
  if ($address -eq [IntPtr]::Zero) {
    $findings.Add([ordered]@{ Kind='GetProcAddressFailure'; Import=$node; Path=$module.Path; Win32=[Runtime.InteropServices.Marshal]::GetLastWin32Error(); Chain=$Chain })
  }
}
try {
  $imports = [CodexxLoaderProbe]::Imports($TestExe)
  foreach ($import in $imports) { Test-ProbeExport $import.Dll $import.Name $import.Ordinal $import.ByOrdinal }
  # A STATUS_ENTRYPOINT_NOT_FOUND can be in a dependency's imports even when
  # the executable's direct imports exist. Traverse the resolved DLL closure.
  $visited = @{}
  $nestedCount = 0
  do {
    $pending = @($modules.Values | Where-Object { $_.Path -and -not $visited.ContainsKey($_.Path.ToLowerInvariant()) })
    foreach ($module in $pending) {
      $visited[$module.Path.ToLowerInvariant()] = $true
      if ($visited.Count -gt 512) { throw 'Dependency traversal exceeded its diagnostic bound.' }
      try { $nested = [CodexxLoaderProbe]::Imports($module.Path) }
      catch { $findings.Add([ordered]@{ Kind='ImportParseFailure'; Path=$module.Path; Detail=$_.Exception.GetType().FullName }); continue }
      $nestedCount += $nested.Count
      foreach ($import in $nested) { Test-ProbeExport $import.Dll $import.Name $import.Ordinal $import.ByOrdinal }
    }
  } while ($pending.Count -gt 0)
  $report['ImportCount'] = $imports.Count
  $report['NestedImportCount'] = $nestedCount
  $report['Findings'] = @($findings.ToArray())
  $report['Modules'] = @($modules.Values | ForEach-Object {
    [ordered]@{ Requested=$_.Requested; LoadArgument=$_.LoadArgument; Path=$_.Path; Version=$_.Version; Sha256=$_.Sha256; LoadError=$_.LoadError; Candidates=$_.Candidates }
  })
  if ($RunHarnessList) {
    $previousMode = [CodexxLoaderProbe]::SetErrorMode(0x8003)
    try {
      $start = @{
        FilePath=$TestExe; ArgumentList='--list'; PassThru=$true; NoNewWindow=$true; Wait=$true
        RedirectStandardOutput=(Join-Path $OutputDirectory 'harness-list.stdout.txt')
        RedirectStandardError=(Join-Path $OutputDirectory 'harness-list.stderr.txt')
      }
      $process = Start-Process @start
      $bits = [BitConverter]::ToUInt32([BitConverter]::GetBytes([int32]$process.ExitCode),0)
      $report['HarnessListExit'] = ('0x{0:X8}' -f $bits)
    } finally { [void][CodexxLoaderProbe]::SetErrorMode($previousMode) }
  }
  Copy-Item -LiteralPath $TestExe -Destination (Join-Path $OutputDirectory (Split-Path -Leaf $TestExe))
  $report | ConvertTo-Json -Depth 12 | Out-File (Join-Path $OutputDirectory 'diagnostics.json') -Encoding utf8
  Write-Host "DLL diagnostics: $OutputDirectory"
  Write-Host "Inspected $($imports.Count) imports; findings: $($findings.Count)"
  foreach ($finding in $findings) { Write-Host ($finding | ConvertTo-Json -Compress -Depth 8) }
} finally {
  foreach ($module in $modules.Values) { if ($module.Handle -ne [IntPtr]::Zero) { [void][CodexxLoaderProbe]::FreeLibrary($module.Handle) } }
}
