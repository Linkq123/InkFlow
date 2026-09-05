[CmdletBinding()]
param(
  [switch]$Check
)

$ErrorActionPreference = 'Stop'
$projectRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$iconDirectory = Join-Path $projectRoot 'src-tauri\icons'
$brandLogo = Join-Path $projectRoot 'logo.png'
$targetRoot = [System.IO.Path]::GetFullPath((Join-Path $projectRoot 'src-tauri\target'))
$temporaryRoot = Join-Path $targetRoot ("inkflow-icons-{0}" -f [guid]::NewGuid().ToString('N'))
$generatedDirectory = Join-Path $temporaryRoot 'generated'
$squareIconSource = Join-Path $temporaryRoot 'inkflow-icon-source.png'

$windowsAssets = @(
  '32x32.png',
  '64x64.png',
  '128x128.png',
  '128x128@2x.png',
  'icon.png',
  'icon.ico',
  'StoreLogo.png',
  'Square30x30Logo.png',
  'Square44x44Logo.png',
  'Square71x71Logo.png',
  'Square89x89Logo.png',
  'Square107x107Logo.png',
  'Square142x142Logo.png',
  'Square150x150Logo.png',
  'Square284x284Logo.png',
  'Square310x310Logo.png'
)

function Get-Sha256([string]$Path) {
  $stream = [System.IO.File]::OpenRead($Path)
  try {
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
      return ([System.BitConverter]::ToString($sha256.ComputeHash($stream))).Replace('-', '')
    } finally {
      $sha256.Dispose()
    }
  } finally {
    $stream.Dispose()
  }
}

if (-not (Test-Path -LiteralPath $brandLogo -PathType Leaf)) {
  throw "Brand logo not found: $brandLogo"
}

New-Item -ItemType Directory -Path $generatedDirectory -Force | Out-Null

try {
  Add-Type -AssemblyName System.Drawing
  $source = [System.Drawing.Bitmap]::new($brandLogo)
  try {
    $side = [Math]::Min($source.Width, $source.Height)
    $sourceX = [Math]::Floor(($source.Width - $side) / 2)
    $sourceY = [Math]::Floor(($source.Height - $side) / 2)
    $square = [System.Drawing.Bitmap]::new($side, $side, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    try {
      $graphics = [System.Drawing.Graphics]::FromImage($square)
      try {
        $graphics.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
        $destinationRectangle = [System.Drawing.Rectangle]::new(0, 0, $side, $side)
        $sourceRectangle = [System.Drawing.Rectangle]::new($sourceX, $sourceY, $side, $side)
        $graphics.DrawImage($source, $destinationRectangle, $sourceRectangle, [System.Drawing.GraphicsUnit]::Pixel)
      } finally {
        $graphics.Dispose()
      }
      $square.Save($squareIconSource, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally {
      $square.Dispose()
    }
  } finally {
    $source.Dispose()
  }

  Push-Location $projectRoot
  try {
    $tauriCommand = Join-Path $projectRoot 'node_modules\.bin\tauri.cmd'
    if (-not (Test-Path -LiteralPath $tauriCommand -PathType Leaf)) {
      throw "Tauri CLI not found: $tauriCommand. Run pnpm install first."
    }
    & $tauriCommand icon $squareIconSource --output $generatedDirectory
    if ($LASTEXITCODE -ne 0) { throw "Tauri icon generation failed with exit code $LASTEXITCODE" }

    & node (Join-Path $PSScriptRoot 'windows-icon.mjs') add-frame (Join-Path $generatedDirectory 'icon.ico') (Join-Path $generatedDirectory '128x128.png') 128
    if ($LASTEXITCODE -ne 0) { throw "ICO frame assembly failed with exit code $LASTEXITCODE" }
  } finally {
    Pop-Location
  }

  foreach ($asset in $windowsAssets) {
    $generated = Join-Path $generatedDirectory $asset
    $destination = Join-Path $iconDirectory $asset
    if (-not (Test-Path -LiteralPath $generated -PathType Leaf)) {
      throw "Tauri did not generate required Windows asset: $asset"
    }

    if ($Check) {
      if (-not (Test-Path -LiteralPath $destination -PathType Leaf)) {
        throw "Generated Windows icon is missing from the repository: $asset"
      }
      $generatedHash = Get-Sha256 $generated
      $destinationHash = Get-Sha256 $destination
      if ($generatedHash -ne $destinationHash) {
        throw "Generated Windows icon is out of date: $asset. Run pnpm icons:windows."
      }
    } else {
      Copy-Item -LiteralPath $generated -Destination $destination -Force
    }
  }

  if ($Check) {
    Write-Output 'Windows icon assets match the centered square crop of logo.png.'
  } else {
    Write-Output 'Windows icon assets generated from the centered square crop of logo.png.'
  }
} finally {
  $resolvedTemporaryRoot = [System.IO.Path]::GetFullPath($temporaryRoot)
  $targetPrefix = $targetRoot.TrimEnd([System.IO.Path]::DirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
  if (-not $resolvedTemporaryRoot.StartsWith($targetPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing to remove temporary directory outside the Tauri target directory: $resolvedTemporaryRoot"
  }
  if (Test-Path -LiteralPath $resolvedTemporaryRoot) {
    Remove-Item -LiteralPath $resolvedTemporaryRoot -Recurse -Force
  }
}
