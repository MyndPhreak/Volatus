<#
.SYNOPSIS
  A/B scan-cost gate: runs the bench of origin/main and of this branch, alternating, and
  fails when the branch is more than 5 % slower in total time.

.DESCRIPTION
  Both builds must exist already (build them in a NORMAL terminal, once in each checkout):
      cargo build -p scan --release --example bench
  For -Method mft, run this script in an ELEVATED PowerShell. The walker needs no elevation.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File docs\bench-ab.ps1 `
      -Main C:\Users\mynd\DEV\flow-main -Branch C:\Users\mynd\DEV\storagetool-delete -Root C:\ -Method mft

.NOTES
  Exit code 0: gate passed. 1: branch total median is over the limit. 2: bad setup.
  Medians are taken over all runs of all rounds (Rounds x Runs rows per build).
#>
param(
    [Parameter(Mandatory = $true)][string]$Main,    # checkout of origin/main with the bench built
    [Parameter(Mandatory = $true)][string]$Branch,  # this checkout with the bench built
    [string]$Root = 'C:\',                          # folder or drive to scan (C:\ with the backslash)
    [ValidateSet('mft', 'walk')][string]$Method = 'mft',
    [int]$Rounds = 3,                               # alternating rounds: main, branch, main, branch, ...
    [int]$Runs = 5,                                 # scans per bench call (bench --runs)
    [double]$LimitPercent = 5.0,                    # gate: branch total median may be this much over main
    [string]$KeepCsvDir = ''                        # when set, copy the two CSVs here before cleanup
)

$ErrorActionPreference = 'Stop'

# Step 1: the MFT path reads the raw volume, so it needs Administrator. Refuse early and clearly.
if ($Method -eq 'mft') {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    $isAdmin = ([Security.Principal.WindowsPrincipal]$id).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    if (-not $isAdmin) {
        [Console]::Error.WriteLine('The mft method needs an elevated PowerShell (Run as administrator). Nothing was run.')
        exit 2
    }
}

# Step 2: find both bench executables.
$exeRel = 'target\release\examples\bench.exe'
$mainExe = Join-Path $Main $exeRel
$branchExe = Join-Path $Branch $exeRel
foreach ($exe in @($mainExe, $branchExe)) {
    if (-not (Test-Path -LiteralPath $exe)) {
        [Console]::Error.WriteLine("Missing $exe. Build it first: cargo build -p scan --release --example bench")
        exit 2
    }
}

# Step 3: one CSV per build, in a fresh temp folder (bench appends one row per run).
$work = Join-Path ([IO.Path]::GetTempPath()) ("bench-ab-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
$csv = @{ main = Join-Path $work 'main.csv'; branch = Join-Path $work 'branch.csv' }

function Invoke-Bench([string]$Exe, [string]$CsvPath) {
    # --method is explicit, so an mft run never silently times the walker.
    & $Exe $Root --method $Method --runs $Runs --csv $CsvPath | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "bench failed (exit $LASTEXITCODE): $Exe" }
}

try {
    # Step 4: one warm-up call per build, not counted (its CSV is thrown away).
    Write-Host "Warm-up ($Method on $Root) ..."
    Invoke-Bench $mainExe (Join-Path $work 'warm-main.csv')
    Invoke-Bench $branchExe (Join-Path $work 'warm-branch.csv')

    # Step 5: alternate main and branch so drift in the machine hits both the same way.
    for ($r = 1; $r -le $Rounds; $r++) {
        Write-Host "Round $r of ${Rounds}: main"
        Invoke-Bench $mainExe $csv.main
        Write-Host "Round $r of ${Rounds}: branch"
        Invoke-Bench $branchExe $csv.branch
    }

    # Step 6: median of a column over all rows.
    function Get-Median([double[]]$v) {
        $s = $v | Sort-Object
        $n = $s.Count
        if ($n % 2 -eq 1) { return [double]$s[($n - 1) / 2] }
        return ([double]$s[$n / 2 - 1] + [double]$s[$n / 2]) / 2.0
    }
    # The CSV columns come from scan/examples/bench.rs (CSV_HEADER).
    $cols = @('open_ms', 'read_ms', 'parse_ms', 'rollup_ms', 'total_ms')
    $med = @{}
    foreach ($k in 'main', 'branch') {
        $rows = Import-Csv -LiteralPath $csv[$k]
        $med[$k] = @{}
        foreach ($c in $cols) { $med[$k][$c] = Get-Median ([double[]]($rows | ForEach-Object { [double]$_.$c })) }
        $med[$k]['n'] = @($rows).Count
    }

    # Step 7: print medians and the branch difference against main.
    Write-Host ''
    Write-Host ("{0,-10}{1,12}{2,12}{3,10}" -f 'phase', 'main ms', 'branch ms', 'diff %')
    foreach ($c in $cols) {
        $m = $med.main[$c]; $b = $med.branch[$c]
        $pct = if ($m -gt 0) { 100.0 * ($b - $m) / $m } else { [double]::NaN }
        Write-Host ("{0,-10}{1,12:N1}{2,12:N1}{3,10:N2}" -f $c.Replace('_ms', ''), $m, $b, $pct)
    }
    Write-Host ("runs per build: main {0}, branch {1}" -f $med.main.n, $med.branch.n)

    # Step 8: the gate, on the total median.
    $mt = $med.main['total_ms']; $bt = $med.branch['total_ms']
    $diff = 100.0 * ($bt - $mt) / $mt
    if ($diff -gt $LimitPercent) {
        Write-Host ("GATE FAILED: branch total is {0:N2} % over main (limit {1} %)." -f $diff, $LimitPercent)
        exit 1
    }
    Write-Host ("Gate passed: branch total is {0:N2} % against main (limit {1} %)." -f $diff, $LimitPercent)
    exit 0
}
finally {
    if ($KeepCsvDir -ne '') {
        New-Item -ItemType Directory -Force -Path $KeepCsvDir | Out-Null
        Copy-Item -LiteralPath $csv.main, $csv.branch -Destination $KeepCsvDir -ErrorAction SilentlyContinue
    }
    # Step 9: remove only the temp folder this script created.
    if (Test-Path -LiteralPath $work) { Remove-Item -LiteralPath $work -Recurse -Force }
}
