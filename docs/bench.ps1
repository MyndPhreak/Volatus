<#
.SYNOPSIS
  M3 benchmark: storagetool's MFT scan against WizTree on the same drive, run after run.

.DESCRIPTION
  Run from an ELEVATED PowerShell (Run as Administrator). Build the bench first from a normal,
  non-elevated prompt, so the target folder stays owned by your user:
      cargo build -p scan --release --example bench

  The script does a warm-up run of each tool, then alternates them for -Rounds rounds, and
  writes everything to bench-results\<time>\results.txt (paste that file back). With -Sweep it
  also times chunk sizes and thread counts. It changes nothing on the drive: both tools only
  read, and the WizTree export goes to the results folder.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File docs\bench.ps1
.EXAMPLE
  powershell -ExecutionPolicy Bypass -File docs\bench.ps1 -Drive D -Rounds 3 -Sweep
#>
param(
    [ValidatePattern('^[A-Za-z]$')][string]$Drive = 'C',
    [ValidateRange(1, 50)][int]$Rounds = 5,
    [switch]$Sweep
)
$ErrorActionPreference = 'Stop'

$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Error 'Run this from an elevated PowerShell (Run as Administrator).'
    exit 1
}
$repo = Split-Path -Parent $PSScriptRoot
$bench = Join-Path $repo 'target\release\examples\bench.exe'
$wiztree = 'C:\Program Files\WizTree\WizTree64.exe'
if (-not (Test-Path $bench)) {
    Write-Error "Not built. From a NON-elevated prompt run: cargo build -p scan --release --example bench"
    exit 1
}
if (-not (Test-Path $wiztree)) {
    Write-Error "WizTree not found at $wiztree"
    exit 1
}

$outDir = Join-Path $repo ("bench-results\" + (Get-Date -Format 'yyyyMMdd-HHmmss'))
New-Item -ItemType Directory -Force $outDir | Out-Null
$log = Join-Path $outDir 'results.txt'
$csv = Join-Path $outDir 'storagetool.csv'
$root = "${Drive}:\"
$tiny = Join-Path $outDir 'tiny'
New-Item -ItemType Directory -Force $tiny | Out-Null
Set-Content (Join-Path $tiny 'a.txt') 'x'

function Say([string]$s) { $s | Tee-Object -FilePath $log -Append }
function Median([double[]]$v) {
    $s = $v | Sort-Object
    $n = $s.Count
    if ($n % 2) { $s[($n - 1) / 2] } else { ($s[$n / 2 - 1] + $s[$n / 2]) / 2 }
}

# storagetool: one in-process run through scan_with (the app's path). Returns the in-process
# total (from the bench) and the wall time of the whole process.
function Run-Ours([string]$path, [string[]]$extra) {
    $ErrorActionPreference = 'Continue' # a line on stderr is output to log, not a script error
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $out = & $bench $path --method mft --runs 1 --csv $csv @extra 2>&1
    $sw.Stop()
    if ($LASTEXITCODE -ne 0) { Say ($out -join "`n"); throw 'storagetool bench failed' }
    $total = [double](($out | Select-String '^total\s+([\d.]+)').Matches[0].Groups[1].Value)
    [pscustomobject]@{ InProcess = $total; Wall = $sw.Elapsed.TotalMilliseconds; Line = ($out | Select-String '^run 1:').Line }
}

# WizTree: wall time of a full CLI run (start, MFT scan, small export of the top folders, exit).
function Run-WizTree([string]$target) {
    $export = Join-Path $outDir 'wiztree.csv'
    $wtArgs = @($target, "/export=`"$export`"", '/admin=1', '/exportfiles=0', '/exportmaxdepth=1')
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $p = Start-Process -FilePath $wiztree -ArgumentList $wtArgs -Wait -PassThru
    $sw.Stop()
    if ($p.ExitCode -ne 0) { throw "WizTree exit code $($p.ExitCode)" }
    $sw.Elapsed.TotalMilliseconds
}

Say "== machine"
Say (Get-CimInstance Win32_Processor | Select-Object -First 1 -ExpandProperty Name)
Say ("RAM {0:N1} GB" -f ((Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory / 1GB))
Say ("Disk: " + (Get-Partition -DriveLetter $Drive | Get-Disk | Select-Object -ExpandProperty FriendlyName))
$os = Get-CimInstance Win32_OperatingSystem
Say "$($os.Caption) build $($os.BuildNumber)"
Say ("WizTree " + (Get-Item $wiztree).VersionInfo.ProductVersion)
fsutil fsinfo ntfsinfo "${Drive}:" | Select-String 'Mft Valid Data Length|Bytes Per Cluster|Bytes Per FileRecord Segment' |
    ForEach-Object { Say $_.Line.Trim() }

Say "== fixed cost: each tool on a one-file folder (3 runs each, wall ms)"
$wtFixed = @(1..3 | ForEach-Object { Run-WizTree $tiny })
Say ("WizTree: " + (($wtFixed | ForEach-Object { '{0:N0}' -f $_ }) -join ', '))

Say "== warm-up (not counted): one run of each on $root"
$w = Run-Ours $root @()
Say $w.Line
Say ("WizTree warm-up wall {0:N0} ms" -f (Run-WizTree "${Drive}:"))

Say "== $Rounds rounds, alternating (ms)"
$ours = @(); $oursWall = @(); $wt = @()
for ($i = 1; $i -le $Rounds; $i++) {
    $o = Run-Ours $root @()
    $t = Run-WizTree "${Drive}:"
    $ours += $o.InProcess; $oursWall += $o.Wall; $wt += $t
    Say ("round {0}: storagetool {1:N0} in-process, {2:N0} wall | WizTree {3:N0} wall" -f $i, $o.InProcess, $o.Wall, $t)
    Say ("  " + $o.Line)
}
Say "== summary (median ms)"
Say ("storagetool in-process  {0:N0}" -f (Median $ours))
Say ("storagetool wall        {0:N0}" -f (Median $oursWall))
Say ("WizTree wall            {0:N0}" -f (Median $wt))
Say ("WizTree fixed cost      {0:N0}  (one-file folder)" -f (Median $wtFixed))
Say ("WizTree minus fixed     {0:N0}" -f ((Median $wt) - (Median $wtFixed)))

if ($Sweep) {
    Say "== chunk sweep: 3 runs each, in-process total ms"
    foreach ($mib in 1, 2, 4, 8, 16, 32, 64) {
        $v = @(1..3 | ForEach-Object { (Run-Ours $root @('--chunk-mib', "$mib")).InProcess })
        Say ("chunk {0,3} MiB: median {1:N0}  ({2})" -f $mib, (Median $v), (($v | ForEach-Object { '{0:N0}' -f $_ }) -join ', '))
    }
    Say "== thread sweep: 3 runs each, in-process total ms"
    foreach ($n in 2, 4, 8, 16, 0) {
        $extra = if ($n -gt 0) { @('--threads', "$n") } else { @() }
        $v = @(1..3 | ForEach-Object { (Run-Ours $root $extra).InProcess })
        $label = if ($n -gt 0) { "$n" } else { 'default' }
        Say ("threads {0,7}: median {1:N0}  ({2})" -f $label, (Median $v), (($v | ForEach-Object { '{0:N0}' -f $_ }) -join ', '))
    }
}
Say "== per-run phases are in $csv"
Say "Done. Paste $log back."
