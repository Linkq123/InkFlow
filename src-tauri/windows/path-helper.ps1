param([ValidateSet('Install', 'Remove')][string]$Action)

function Get-InkFlowPathChange {
    param(
        [ValidateSet('Install', 'Remove')][string]$Action,
        [bool]$Exists,
        [AllowEmptyString()][string]$Value,
        [string]$Entry,
        [bool]$OriginalExisted = $true
    )
    $parts = @($Value.Split(';'))
    $matches = @(for ($i = 0; $i -lt $parts.Count; $i++) {
        if ([string]::Equals($parts[$i], $Entry, [StringComparison]::OrdinalIgnoreCase)) { $i }
    })
    if ($Action -eq 'Install') {
        if ($matches.Count -gt 0) { return [pscustomobject]@{ Code = 2; Exists = $Exists; Value = $Value } }
        $next = if ([string]::IsNullOrEmpty($Value)) { $Entry }
            elseif ($Value.EndsWith(';')) { $Value + $Entry + ';' }
            else { $Value + ';' + $Entry }
        return [pscustomobject]@{ Code = 0; Exists = $true; Value = $next }
    }
    if ($matches.Count -eq 0) { return [pscustomobject]@{ Code = 2; Exists = $Exists; Value = $Value } }
    if ($matches.Count -ne 1) { return [pscustomobject]@{ Code = 3; Exists = $Exists; Value = $Value } }
    $kept = @(for ($i = 0; $i -lt $parts.Count; $i++) { if ($i -ne $matches[0]) { $parts[$i] } })
    return [pscustomobject]@{
        Code = 0
        Exists = ($OriginalExisted -or $kept.Count -gt 0)
        Value = [string]::Join(';', $kept)
    }
}

# Dot-sourcing without an action exposes the real transformation to contract tests.
if (-not $Action) { return }
$ErrorActionPreference = 'Stop'
$key = $null
try {
    $entry = [Environment]::GetEnvironmentVariable('INKFLOW_PATH_ENTRY', 'Process')
    if ([string]::IsNullOrEmpty($entry)) { throw 'The installation directory was not provided.' }
    $originalExisted = [Environment]::GetEnvironmentVariable('INKFLOW_PATH_EXISTED', 'Process') -ne '0'
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
    if ($null -eq $key) { throw 'The user environment registry key is unavailable.' }
    $exists = $key.GetValueNames() -contains 'Path'
    $value = [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    $change = Get-InkFlowPathChange -Action $Action -Exists $exists -Value $value -Entry $entry -OriginalExisted $originalExisted
    if ($change.Code -ne 0) { exit $change.Code }
    if ($change.Exists) {
        $kind = if ($exists) { $key.GetValueKind('Path') } else { [Microsoft.Win32.RegistryValueKind]::ExpandString }
        $key.SetValue('Path', $change.Value, $kind)
    } else {
        $key.DeleteValue('Path', $false)
    }
    if ($Action -eq 'Install') { [Console]::Out.Write([int]$exists) }
    exit 0
} catch {
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
} finally {
    if ($null -ne $key) { $key.Dispose() }
}
