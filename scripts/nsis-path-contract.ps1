$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '../src-tauri/windows/path-helper.ps1')
$entry = 'C:\Program Files\InkFlow'
$cases = @(
    @{ Exists = $false; Value = '' },
    @{ Exists = $true; Value = '' },
    @{ Exists = $true; Value = 'C:\Windows' },
    @{ Exists = $true; Value = 'C:\Windows;' },
    @{ Exists = $true; Value = 'C:\Windows;;' },
    @{ Exists = $true; Value = ('C:\long-path;' * 300) + '%USERPROFILE%\bin' }
)
foreach ($case in $cases) {
    $installed = Get-InkFlowPathChange -Action Install -Exists $case.Exists -Value $case.Value -Entry $entry
    $removed = Get-InkFlowPathChange -Action Remove -Exists $true -Value $installed.Value -Entry $entry -OriginalExisted $case.Exists
    if ($installed.Code -ne 0 -or $removed.Code -ne 0 -or $removed.Exists -ne $case.Exists -or $removed.Value -cne $case.Value) { throw 'PATH did not round-trip.' }
}
$installed = Get-InkFlowPathChange -Action Install -Exists $true -Value 'C:\Windows' -Entry $entry
foreach ($value in @(($installed.Value + ';' + $entry), ($entry + ';' + $installed.Value), ($installed.Value + ';' + $entry.ToLowerInvariant()))) {
    $removed = Get-InkFlowPathChange -Action Remove -Exists $true -Value $value -Entry $entry
    if ($removed.Code -ne 3 -or $removed.Value -cne $value) { throw 'An ambiguous PATH entry was changed.' }
}
$existing = Get-InkFlowPathChange -Action Install -Exists $true -Value $installed.Value -Entry $entry
$missing = Get-InkFlowPathChange -Action Remove -Exists $true -Value 'C:\Windows' -Entry $entry
if ($existing.Code -ne 2 -or $missing.Code -ne 2) { throw 'No-op PATH results were incorrect.' }
Write-Output 'PowerShell PATH contract passed.'
