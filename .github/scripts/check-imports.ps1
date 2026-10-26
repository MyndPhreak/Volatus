# Fails when an exe imports the Visual C++ runtime (VCRUNTIME*.dll, MSVCP*.dll, the UCRT's
# ucrtbase.dll / api-ms-win-crt-*.dll): the MSI ships only volatus.exe, which is built with the
# static CRT (.cargo/config.toml) so it runs without the Visual C++ Redistributable.
# Reads the PE import and delay-import tables itself (no dumpbin or Visual Studio environment).
# Also fails unless the PE Subsystem is IMAGE_SUBSYSTEM_WINDOWS_GUI (2): a console-subsystem exe
# flashes a cmd window when Explorer, the Start menu or the MSI launches it.
# Copied from Negotium's .github/scripts/check-imports.ps1.
# Usage: pwsh .github/scripts/check-imports.ps1 <path to exe>
param([Parameter(Mandatory = $true)][string]$Exe)
$ErrorActionPreference = 'Stop'

$bytes = [System.IO.File]::ReadAllBytes((Resolve-Path $Exe))
function U16([int]$o) { [BitConverter]::ToUInt16($bytes, $o) }
function U32([int]$o) { [BitConverter]::ToUInt32($bytes, $o) }

if ((U16 0) -ne 0x5A4D) { throw "$Exe is not a PE file (no MZ header)" }
$pe = [int](U32 0x3C)
if ((U32 $pe) -ne 0x00004550) { throw "$Exe is not a PE file (no PE signature)" }
$sectionCount = U16 ($pe + 6)
$optionalSize = U16 ($pe + 20)
$optional = $pe + 24
$magic = U16 $optional
switch ($magic) {
  0x20B { $dirs = $optional + 112 } # PE32+
  0x10B { $dirs = $optional + 96 }  # PE32
  default { throw "unknown optional header magic 0x$('{0:X}' -f $magic)" }
}
# Optional header Subsystem (offset 68 in PE32 and PE32+): 2 = Windows GUI, 3 = console.
$subsystem = U16 ($optional + 68)
$sections = $optional + $optionalSize

function Offset([uint32]$rva) {
  for ($i = 0; $i -lt $sectionCount; $i++) {
    $s = $sections + 40 * $i
    $va = U32 ($s + 12); $size = [Math]::Max((U32 ($s + 8)), (U32 ($s + 16))); $raw = U32 ($s + 20)
    if ($rva -ge $va -and $rva -lt $va + $size) { return [int]($rva - $va + $raw) }
  }
  throw "RVA 0x$('{0:X}' -f $rva) is in no section"
}
function Name([uint32]$rva) {
  $o = Offset $rva; $end = $o
  while ($bytes[$end] -ne 0) { $end++ }
  [System.Text.Encoding]::ASCII.GetString($bytes, $o, $end - $o)
}

$dlls = @()
# Data directory 1: imports (20-byte descriptors, name RVA at +12); 13: delay imports (32-byte
# descriptors, name RVA at +4). Each list ends with an all-zero descriptor.
foreach ($table in @(@{ Index = 1; Size = 20; Name = 12 }, @{ Index = 13; Size = 32; Name = 4 })) {
  $rva = U32 ($dirs + 8 * $table.Index)
  if ($rva -eq 0) { continue }
  $o = Offset $rva
  while ($true) {
    $nameRva = U32 ($o + $table.Name)
    if ($nameRva -eq 0) { break }
    $dlls += Name $nameRva
    $o += $table.Size
  }
}

if ($dlls.Count -eq 0) { throw "no imports found in ${Exe}: the check itself is broken" }
Write-Host "$Exe imports: $($dlls -join ', ')"
$crt = @($dlls | Where-Object { $_ -match '^(vcruntime|msvcp|ucrtbase|api-ms-win-crt-)' })
if ($crt.Count -gt 0) {
  throw "$Exe imports the Visual C++ runtime ($($crt -join ', ')); build with the static CRT (.cargo/config.toml)"
}
Write-Host "No Visual C++ runtime imports."
if ($subsystem -ne 2) {
  throw "$Exe has PE subsystem $subsystem, not 2 (Windows GUI): it opens a console window; set windows_subsystem to windows for release builds"
}
Write-Host "PE subsystem is Windows GUI (2)."
