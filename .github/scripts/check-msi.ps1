# Checks the tables of a built Volatus MSI (opened read-only; nothing is installed). Only the built
# MSI shows what light really wrote: the CloseApplication step's id and sequence number, and the
# Registry rows. app/tests/wix.rs checks the source; this checks the result.
# Usage: pwsh .github/scripts/check-msi.ps1 <path to msi> <X.Y.Z>
param(
  [Parameter(Mandatory = $true)][string]$Msi,
  [Parameter(Mandatory = $true)][string]$Version
)
$ErrorActionPreference = 'Stop'

$UpgradeCode = '{FB5B570F-DD29-49B8-A6AE-35855278CAE6}'
# The x64 id of WixUtilExtension's CloseApplication step (captured from the built MSI).
$CloseAction = 'WixCloseApplications_x64'
$Exe = '[#VolatusExe]'
$MenuKeys = [ordered]@{
  'Software\Classes\Directory\shell\Volatus'            = '%1'
  'Software\Classes\Directory\Background\shell\Volatus' = '%V'
  'Software\Classes\Drive\shell\Volatus'                = '%1'
}

# The Windows Installer COM object has no type library for PowerShell: call it by name.
function Invoke([object]$Target, [string]$Name, [object[]]$Arguments, [string]$Kind = 'InvokeMethod') {
  $Target.GetType().InvokeMember($Name, $Kind, $null, $Target, $Arguments)
}

$installer = New-Object -ComObject WindowsInstaller.Installer
$db = Invoke $installer 'OpenDatabase' @([string](Resolve-Path $Msi).Path, [int]0) # 0: read-only

# Every row of a query, each as an array of its column strings.
function Rows([string]$Query) {
  $view = Invoke $db 'OpenView' @($Query)
  Invoke $view 'Execute' @() | Out-Null
  $count = [int](Invoke (Invoke $view 'ColumnInfo' @([int]0) 'GetProperty') 'FieldCount' @() 'GetProperty')
  $rows = [System.Collections.Generic.List[object]]::new()
  while ($null -ne ($record = Invoke $view 'Fetch' @())) {
    $rows.Add([string[]]@(for ($i = 1; $i -le $count; $i++) { Invoke $record 'StringData' @([int]$i) 'GetProperty' }))
  }
  Invoke $view 'Close' @() | Out-Null
  , $rows
}

function Check([bool]$Ok, [string]$What) {
  if (-not $Ok) { throw "check-msi: $What" }
  Write-Host "OK  $What"
}

function Property([string]$Name) {
  $rows = Rows "SELECT ``Value`` FROM ``Property`` WHERE ``Property`` = '$Name'"
  if ($rows.Count -eq 1) { $rows[0][0] } else { $null }
}

function SequenceOf([string]$Action) {
  $rows = Rows "SELECT ``Sequence`` FROM ``InstallExecuteSequence`` WHERE ``Action`` = '$Action'"
  if ($rows.Count -ne 1) { throw "check-msi: $Action is not in InstallExecuteSequence once" }
  [int]$rows[0][0]
}

# Product
Check ((Property 'ProductVersion') -eq $Version) "ProductVersion = $Version (found $(Property 'ProductVersion'))"
Check ((Property 'UpgradeCode') -eq $UpgradeCode) "UpgradeCode = $UpgradeCode"
Check ((Property 'ProductName') -eq 'Volatus') 'ProductName = Volatus'
Check ((Property 'Manufacturer') -eq 'MyndPhreak') 'Manufacturer = MyndPhreak'
Check ((Property 'ALLUSERS') -eq '1') 'ALLUSERS = 1 (per-machine)'
$template = Invoke (Invoke $db 'SummaryInformation' @([int]0) 'GetProperty') 'Property' @([int]7) 'GetProperty'
Check ($template -eq 'x64;1033') "Template = x64;1033 (found $template)"

# Files and shortcut
$files = Rows 'SELECT `File`, `Component_`, `FileName` FROM `File`'
Check ($files.Count -eq 1 -and $files[0][0] -eq 'VolatusExe' -and $files[0][2] -match '(^|\|)volatus\.exe$') "one File: VolatusExe = volatus.exe ($($files[0][2]))"
$folder = Rows "SELECT ``Directory_Parent``, ``DefaultDir`` FROM ``Directory`` WHERE ``Directory`` = 'INSTALLFOLDER'"
Check ($folder.Count -eq 1 -and $folder[0][0] -eq 'ProgramFiles64Folder' -and $folder[0][1] -eq 'Volatus') 'INSTALLFOLDER = ProgramFiles64Folder\Volatus'
$shortcuts = Rows 'SELECT `Directory_`, `Name`, `Target` FROM `Shortcut`'
Check ($shortcuts.Count -eq 1 -and $shortcuts[0][0] -eq 'ProgramMenuFolder' -and $shortcuts[0][1] -match 'Volatus$' -and $shortcuts[0][2] -eq $Exe) "one Shortcut: ProgramMenuFolder\Volatus -> $Exe"

# Upgrades and the launch condition
$upgrades = Rows "SELECT ``VersionMin``, ``VersionMax``, ``Attributes``, ``ActionProperty`` FROM ``Upgrade`` WHERE ``UpgradeCode`` = '$UpgradeCode'"
$replace = @($upgrades | Where-Object { $_[3] -eq 'WIX_UPGRADE_DETECTED' })
$refuse = @($upgrades | Where-Object { $_[3] -eq 'WIX_DOWNGRADE_DETECTED' })
# 513 = 512 VersionMaxInclusive (same version replaces) + 1 MigrateFeatures (the menu's state is
# kept); 2 = OnlyDetect (a newer version is only found, and refused below).
Check ($upgrades.Count -eq 2 -and $replace.Count -eq 1 -and $replace[0][0] -eq '' -and $replace[0][1] -eq $Version -and $replace[0][2] -eq '513') "Upgrade: versions up to $Version replaced, features migrated (Attributes 513)"
Check ($refuse.Count -eq 1 -and $refuse[0][0] -eq $Version -and $refuse[0][1] -eq '' -and $refuse[0][2] -eq '2') "Upgrade: versions above $Version only detected (Attributes 2)"
$launch = Rows 'SELECT `Condition`, `Description` FROM `LaunchCondition`'
$conditions = $launch | ForEach-Object { $_[0] }
Check ($launch.Count -eq 2 -and $conditions -contains 'NOT WIX_DOWNGRADE_DETECTED') 'LaunchCondition: NOT WIX_DOWNGRADE_DETECTED (downgrade refused)'
$win10 = @($launch | Where-Object { $_[0] -eq 'Installed OR VOLATUS_WIN10' })
Check ($win10.Count -eq 1 -and $win10[0][1] -eq 'Volatus needs 64-bit Windows 10 or newer.') 'LaunchCondition: Installed OR VOLATUS_WIN10, "Volatus needs 64-bit Windows 10 or newer."'
$search = Rows "SELECT ``Root``, ``Key``, ``Name``, ``Type`` FROM ``RegLocator`` WHERE ``Signature_`` = 'WindowsMajorVersionSearch'"
Check ($search.Count -eq 1 -and $search[0][0] -eq '2' -and $search[0][1] -eq 'SOFTWARE\Microsoft\Windows NT\CurrentVersion' -and $search[0][2] -eq 'CurrentMajorVersionNumber' -and $search[0][3] -eq '18') 'RegLocator: HKLM 64-bit (type 2 + 16) CurrentVersion\CurrentMajorVersionNumber'

# Installed apps
Check ((Property 'ARPNOMODIFY') -eq '1') 'ARPNOMODIFY = 1'
Check ((Property 'ARPPRODUCTICON') -eq 'volatus.ico') 'ARPPRODUCTICON = volatus.ico'
Check ((Property 'ARPURLINFOABOUT') -eq 'https://github.com/MyndPhreak/Volatus') 'ARPURLINFOABOUT'

# The running copy is asked to close before InstallValidate, before anything is removed or copied.
$close = SequenceOf $CloseAction
$validate = SequenceOf 'InstallValidate'
$initialize = SequenceOf 'InstallInitialize'
$remove = SequenceOf 'RemoveExistingProducts'
$install = SequenceOf 'InstallFiles'
Check ($close -lt $validate -and $validate -lt $initialize -and $initialize -lt $remove -and $remove -lt $install) "sequence: $CloseAction $close < InstallValidate $validate < InstallInitialize $initialize < RemoveExistingProducts $remove < InstallFiles $install"
$deferred = Rows "SELECT ``Action`` FROM ``InstallExecuteSequence`` WHERE ``Action`` = '${CloseAction}Deferred' OR ``Action`` = 'WixCloseApplicationsDeferred_x64' OR ``Action`` = 'WixCloseApplicationsDeferred'"
Check ($deferred.Count -eq 0) 'no deferred close step in InstallExecuteSequence'
$apps = Rows 'SELECT `Target`, `Attributes`, `Timeout` FROM `WixCloseApplication`'
# Attributes 1 = CloseMessage only: no RebootPrompt (2), no Elevated* (4, 16), EndSession (8),
# TerminateProcess (32) or PromptToContinue (64).
Check ($apps.Count -eq 1 -and $apps[0][0] -eq 'volatus.exe' -and $apps[0][1] -eq '1') "WixCloseApplication: volatus.exe, Attributes 1 (found $($apps[0][1]))"

# The Explorer menu: HKLM only, under the three Volatus keys, with the contract's values.
$registry = Rows 'SELECT `Root`, `Key`, `Name`, `Value`, `Component_` FROM `Registry`'
foreach ($row in $registry) {
  $under = @($MenuKeys.Keys | Where-Object { $row[1] -eq $_ -or $row[1] -eq "$_\command" })
  Check ($row[0] -eq '2' -and $under.Count -eq 1) "Registry row HKLM\$($row[1]) [$($row[2])] is a menu key"
}
$want = foreach ($key in $MenuKeys.Keys) {
  "$key||Scan with Volatus"
  "$key|Icon|$Exe,0"
  "$key\command||`"$Exe`" `"$($MenuKeys[$key])`""
  "$key|*|"
}
$found = $registry | ForEach-Object { "$($_[1])|$($_[2])|$($_[3])" }
$missing = @($want | Where-Object { $found -notcontains $_ })
Check ($registry.Count -eq 12 -and $missing.Count -eq 0) "Registry: the 12 menu rows (3 keys x label, Icon, command, create/delete)$(if ($missing) { "; missing $($missing -join ', ')" })"
$features = Rows "SELECT ``Feature_Parent``, ``Level`` FROM ``Feature`` WHERE ``Feature`` = 'ExplorerMenu'"
Check ($features.Count -eq 1 -and $features[0][0] -eq 'Main' -and $features[0][1] -eq '1') 'Feature ExplorerMenu: child of Main, Level 1'

Write-Host "check-msi: $Msi passed."
