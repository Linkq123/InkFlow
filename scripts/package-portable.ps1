param(
    [string]$OutputDirectory = "release"
)

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
$executable = Join-Path $projectRoot "src-tauri\target\release\inkflow.exe"
if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
    throw "Release executable not found. Run 'pnpm tauri build --no-bundle' first."
}

$resolvedOutput = [System.IO.Path]::GetFullPath((Join-Path $projectRoot $OutputDirectory))
$resolvedRoot = [System.IO.Path]::GetFullPath($projectRoot)
if (-not $resolvedOutput.StartsWith($resolvedRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Output directory must stay inside the InkFlow project."
}

New-Item -ItemType Directory -Path $resolvedOutput -Force | Out-Null
$staging = Join-Path $resolvedOutput "InkFlow-portable"
if (Test-Path -LiteralPath $staging) {
    Remove-Item -LiteralPath $staging -Recurse -Force
}
New-Item -ItemType Directory -Path $staging | Out-Null
Copy-Item -LiteralPath $executable -Destination (Join-Path $staging "InkFlow.exe")
Copy-Item -LiteralPath (Join-Path $projectRoot "README.md") -Destination $staging

$archive = Join-Path $resolvedOutput "InkFlow-0.1.0-windows-x64-portable.zip"
if (Test-Path -LiteralPath $archive) {
    Remove-Item -LiteralPath $archive -Force
}
Compress-Archive -Path (Join-Path $staging "*") -DestinationPath $archive -CompressionLevel Optimal
Remove-Item -LiteralPath $staging -Recurse -Force
Write-Output $archive
