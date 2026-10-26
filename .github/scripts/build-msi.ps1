# Builds and checks the Volatus MSI: the one script CI (ci.yml) and the release
# (release-please.yml) run, so they can't drift. Steps: check the version against the package that
# has the volatus bin, download WiX 3.14 (SHA-256 checked before it is unpacked), install
# cargo-wix, build the release exe, check its imports, build the MSI, check its tables.
# Writes exactly one target/wix/volatus-<Version>-x86_64.msi and, on GitHub Actions, MSI_PATH to
# $env:GITHUB_ENV. Any failure stops the script with a non-zero exit code.
# Usage: pwsh .github/scripts/build-msi.ps1 -Version X.Y.Z [-ToolsDir <folder for WiX and cargo-wix>]
param(
  [Parameter(Mandatory = $true)][string]$Version,
  [string]$ToolsDir = $(if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { Join-Path ([IO.Path]::GetTempPath()) 'volatus-msi-tools' })
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

# Negotium's pin (proven by its releases): the wix3141rtm binaries.
$WixTag = 'wix3141rtm'
$WixSha256 = '6AC824E1642D6F7277D0ED7EA09411A508F6116BA6FAE0AA5F2C7DAA2FF43D31'
$CargoWixVersion = '0.3.9'

function Run([string]$Exe, [string[]]$Arguments) {
  & $Exe @Arguments
  if ($LASTEXITCODE -ne 0) { throw "$Exe $($Arguments -join ' ') failed with exit code $LASTEXITCODE" }
}

Set-Location (Join-Path $PSScriptRoot '../..')

# MSI versions are X.Y.Z with X and Y up to 255 and Z up to 65535; cargo-wix checks the range.
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Version '$Version' is not X.Y.Z" }
$metadata = cargo metadata --no-deps --format-version 1
if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed with exit code $LASTEXITCODE" }
$packages = @(($metadata | ConvertFrom-Json).packages | Where-Object {
    $_.targets | Where-Object { $_.name -eq 'volatus' -and $_.kind -contains 'bin' }
  })
if ($packages.Count -ne 1) { throw "expected one package with the bin target volatus, found $($packages.Count)" }
$package = $packages[0]
if ($package.version -ne $Version) {
  throw "Version $Version does not match the $($package.name) package's version $($package.version)"
}
Write-Host "Package $($package.name) $Version"

# WiX: downloaded fresh, unpacked only after its hash matches.
New-Item -ItemType Directory -Force -Path $ToolsDir | Out-Null
$zip = Join-Path $ToolsDir 'wix314-binaries.zip'
$wix = Join-Path $ToolsDir 'wix314'
# Three tries, 5 s then 10 s apart: one network fault does not fail the build.
for ($try = 1; ; $try++) {
  try {
    Invoke-WebRequest -Uri "https://github.com/wixtoolset/wix3/releases/download/$WixTag/wix314-binaries.zip" -OutFile $zip
    break
  } catch {
    if ($try -ge 3) { throw }
    Write-Host "WiX download failed (try $try of 3): $($_.Exception.Message)"
    Start-Sleep -Seconds (5 * $try)
  }
}
$actual = (Get-FileHash -Path $zip -Algorithm SHA256).Hash
if ($actual -ne $WixSha256) { throw "WiX archive SHA-256 mismatch: expected $WixSha256, got $actual" }
if (Test-Path $wix) { Remove-Item -Recurse -Force $wix }
Expand-Archive -Path $zip -DestinationPath $wix
foreach ($exe in 'candle.exe', 'light.exe') {
  if (-not (Test-Path (Join-Path $wix $exe))) { throw "$exe missing from the WiX archive" }
}

# cargo-wix from crates.io with its lockfile, not a prebuilt binary; kept out of PATH.
$cargoWixRoot = Join-Path $ToolsDir 'cargo-wix'
Run cargo @('install', 'cargo-wix', '--locked', '--version', $CargoWixVersion, '--root', $cargoWixRoot)
$cargoWix = Join-Path $cargoWixRoot 'bin/cargo-wix.exe'

Run cargo @('build', '--release', '-p', $package.name)
& (Join-Path $PSScriptRoot 'check-imports.ps1') 'target/release/volatus.exe'

Remove-Item -Path 'target/wix/*.msi' -ErrorAction SilentlyContinue
Run $cargoWix @('wix', '-p', $package.name, '--no-build', '--nocapture', '--bin-path', $wix)
$msi = @(Get-ChildItem -Path 'target/wix' -Filter '*.msi')
if ($msi.Count -ne 1) { throw "expected exactly one MSI in target/wix, found $($msi.Count)" }
$expected = "volatus-$Version-x86_64.msi"
if ($msi[0].Name -ne $expected) { throw "unexpected MSI name $($msi[0].Name), expected $expected" }

& (Join-Path $PSScriptRoot 'check-msi.ps1') $msi[0].FullName $Version

if ($env:GITHUB_ENV) { "MSI_PATH=$($msi[0].FullName)" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8 }
Write-Host "Built $($msi[0].FullName) ($([Math]::Round($msi[0].Length / 1MB, 1)) MB)"
