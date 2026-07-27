param(
    [string]$OutputDirectory = "release",
    [string]$ProjectRoot = ""
)

$ErrorActionPreference = "Stop"
$resolvedProjectRoot = if ([string]::IsNullOrWhiteSpace($ProjectRoot)) {
    [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
} else {
    [System.IO.Path]::GetFullPath($ProjectRoot)
}
$configurationPath = Join-Path $resolvedProjectRoot "src-tauri\tauri.conf.json"
$configuration = Get-Content -LiteralPath $configurationPath -Raw | ConvertFrom-Json
$version = [string]$configuration.version
if ($version -notmatch '^\d+\.\d+\.\d+$') {
    throw "Invalid Tauri application version: '$version'."
}
$executable = Join-Path $resolvedProjectRoot "src-tauri\target\release\inkflow.exe"
if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
    throw "Release executable not found. Run 'pnpm tauri build --no-bundle' first."
}

$resolvedOutput = [System.IO.Path]::GetFullPath((Join-Path $resolvedProjectRoot $OutputDirectory))
$resolvedRoot = $resolvedProjectRoot
$rootPrefix = $resolvedRoot.TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
if ($resolvedOutput -ne $resolvedRoot -and -not $resolvedOutput.StartsWith($rootPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Output directory must stay inside the InkFlow project."
}

New-Item -ItemType Directory -Path $resolvedOutput -Force | Out-Null
$staging = Join-Path $resolvedOutput "InkFlow-portable"
if (Test-Path -LiteralPath $staging) {
    Remove-Item -LiteralPath $staging -Recurse -Force
}
New-Item -ItemType Directory -Path $staging | Out-Null
Copy-Item -LiteralPath $executable -Destination (Join-Path $staging "InkFlow.exe")
Copy-Item -LiteralPath (Join-Path $resolvedProjectRoot "README.md") -Destination $staging

$archive = Join-Path $resolvedOutput "InkFlow-$version-windows-x64-portable.zip"
if (Test-Path -LiteralPath $archive) {
    Remove-Item -LiteralPath $archive -Force
}
Compress-Archive -Path (Join-Path $staging "*") -DestinationPath $archive -CompressionLevel Optimal
Remove-Item -LiteralPath $staging -Recurse -Force
Write-Output $archive
