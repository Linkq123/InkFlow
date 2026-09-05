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
$bundleConfigurationPath = Join-Path $resolvedProjectRoot "src-tauri\tauri.bundle.conf.json"
$cliRequired = $false
if (Test-Path -LiteralPath $bundleConfigurationPath -PathType Leaf) {
    $bundleConfiguration = Get-Content -LiteralPath $bundleConfigurationPath -Raw | ConvertFrom-Json
    $cliRequired = @($bundleConfiguration.bundle.externalBin) -contains "binaries/inkflow-cli"
}
$cliExecutable = Join-Path $resolvedProjectRoot "src-tauri\binaries\inkflow-cli-x86_64-pc-windows-msvc.exe"
if ($cliRequired) {
    $cliBuildScript = Join-Path $resolvedProjectRoot "scripts\prepare-cli-sidecar.ps1"
    if (-not (Test-Path -LiteralPath $cliBuildScript -PathType Leaf)) {
        throw "CLI sidecar build script not found at '$cliBuildScript'."
    }
    # The sidecar directory is ignored by Git. Always rebuild here so a clean
    # checkout works and a stale executable can never enter the portable ZIP.
    $null = & $cliBuildScript -ProjectRoot $resolvedProjectRoot
    if (-not (Test-Path -LiteralPath $cliExecutable -PathType Leaf)) {
        throw "CLI sidecar build completed without producing '$cliExecutable'."
    }
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
if ($cliRequired) {
    Copy-Item -LiteralPath $cliExecutable -Destination (Join-Path $staging "inkflow-cli.exe")
}
$readmePath = Join-Path $resolvedProjectRoot "README.md"
Copy-Item -LiteralPath $readmePath -Destination $staging
$readmeText = Get-Content -LiteralPath $readmePath -Raw
$brandLogoPattern = '<img\b[^>]*\bsrc\s*=\s*["'']logo\.png["'']'
$referencesBrandLogo = [System.Text.RegularExpressions.Regex]::IsMatch(
    $readmeText,
    $brandLogoPattern,
    [System.Text.RegularExpressions.RegexOptions]::IgnoreCase
)
if ($referencesBrandLogo) {
    $logoPath = Join-Path $resolvedProjectRoot "logo.png"
    if (-not (Test-Path -LiteralPath $logoPath -PathType Leaf)) {
        throw "README.md references logo.png, but the brand asset is missing at '$logoPath'."
    }
    Copy-Item -LiteralPath $logoPath -Destination $staging
}

$archive = Join-Path $resolvedOutput "InkFlow-$version-windows-x64-portable.zip"
if (Test-Path -LiteralPath $archive) {
    Remove-Item -LiteralPath $archive -Force
}
Compress-Archive -Path (Join-Path $staging "*") -DestinationPath $archive -CompressionLevel Optimal
Remove-Item -LiteralPath $staging -Recurse -Force
Write-Output $archive
