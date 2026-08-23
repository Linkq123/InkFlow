param(
    [string]$ProjectRoot = "",
    [string]$TargetTriple = "x86_64-pc-windows-msvc",
    [switch]$DebugBuild
)

$ErrorActionPreference = "Stop"
$resolvedProjectRoot = if ([string]::IsNullOrWhiteSpace($ProjectRoot)) {
    [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
} else {
    [System.IO.Path]::GetFullPath($ProjectRoot)
}
$manifest = Join-Path $resolvedProjectRoot "src-tauri\Cargo.toml"
if (-not (Test-Path -LiteralPath $manifest -PathType Leaf)) {
    throw "InkFlow Cargo manifest not found at '$manifest'."
}
if ($TargetTriple -ne "x86_64-pc-windows-msvc") {
    throw "InkFlow v1 only packages the x86_64-pc-windows-msvc CLI."
}

$profile = if ($DebugBuild) { "debug" } else { "release" }
$cargoArguments = @(
    "build",
    "--manifest-path", $manifest,
    "--bin", "inkflow-cli",
    "--no-default-features",
    "--features", "cli",
    "--target", $TargetTriple
)
if (-not $DebugBuild) {
    $cargoArguments += "--release"
}
& cargo @cargoArguments
if ($LASTEXITCODE -ne 0) {
    throw "inkflow-cli build failed with exit code $LASTEXITCODE."
}

$source = Join-Path $resolvedProjectRoot "src-tauri\target\$TargetTriple\$profile\inkflow-cli.exe"
if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
    throw "Built CLI executable not found at '$source'."
}
$binaryDirectory = Join-Path $resolvedProjectRoot "src-tauri\binaries"
New-Item -ItemType Directory -Path $binaryDirectory -Force | Out-Null
$destination = Join-Path $binaryDirectory "inkflow-cli-$TargetTriple.exe"
Copy-Item -LiteralPath $source -Destination $destination -Force
Write-Output $destination
