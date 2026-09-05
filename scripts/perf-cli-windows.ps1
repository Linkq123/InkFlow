param(
  [string]$CliExecutable = (Join-Path $PSScriptRoot "..\src-tauri\binaries\inkflow-cli-x86_64-pc-windows-msvc.exe"),
  [string]$DesktopExecutable = (Join-Path $PSScriptRoot "..\src-tauri\target\release\inkflow.exe"),
  [string]$Fixture = (Join-Path $env:TEMP "inkflow-100k-lines.md"),
  [string]$OutputPath = (Join-Path $PSScriptRoot "..\src-tauri\target\cli-performance.json"),
  [int]$Iterations = 20,
  [int]$RenderIterations = 5
)

$ErrorActionPreference = "Stop"

function ConvertTo-ProcessArgument {
  param([string]$Value)

  if ($Value.Length -eq 0) {
    return '""'
  }
  if ($Value -notmatch '[\s"]') {
    return $Value
  }
  if ($Value.Contains('"')) {
    throw "Performance script arguments cannot contain a double quote."
  }
  $escaped = $Value -replace '(\\+)$', '$1$1'
  return '"' + $escaped + '"'
}

function Invoke-InkFlowCli {
  param(
    [string[]]$Arguments,
    [string]$DataDirectory,
    [string]$DesktopPath = ""
  )

  $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
  $startInfo.FileName = $script:resolvedCli
  $startInfo.UseShellExecute = $false
  $startInfo.CreateNoWindow = $true
  $startInfo.RedirectStandardOutput = $true
  $startInfo.RedirectStandardError = $true
  $allArguments = @("--format", "json", "--data-dir", $DataDirectory) + $Arguments
  $startInfo.Arguments = ($allArguments | ForEach-Object {
    ConvertTo-ProcessArgument -Value $_
  }) -join " "
  if (-not [string]::IsNullOrWhiteSpace($DesktopPath)) {
    $startInfo.EnvironmentVariables["INKFLOW_DESKTOP_EXE"] = $DesktopPath
  }

  $timer = [System.Diagnostics.Stopwatch]::StartNew()
  $process = [System.Diagnostics.Process]::Start($startInfo)
  $stdoutTask = $process.StandardOutput.ReadToEndAsync()
  $stderrTask = $process.StandardError.ReadToEndAsync()
  $process.WaitForExit()
  $stdout = $stdoutTask.GetAwaiter().GetResult()
  $stderr = $stderrTask.GetAwaiter().GetResult()
  $timer.Stop()
  if ($process.ExitCode -ne 0) {
    throw "CLI command failed with exit code $($process.ExitCode): $stderr $stdout"
  }
  [pscustomobject]@{
    elapsedMs = [Math]::Round($timer.Elapsed.TotalMilliseconds, 2)
    stdout = $stdout
  }
}

function Get-Percentile95 {
  param([double[]]$Values)

  $sorted = @($Values | Sort-Object)
  $index = [Math]::Max(0, [Math]::Ceiling($sorted.Count * 0.95) - 1)
  [Math]::Round($sorted[$index], 2)
}

if ($Iterations -lt 5 -or $RenderIterations -lt 1) {
  throw "Use at least 5 startup iterations and 1 render iteration."
}
if (-not (Test-Path -LiteralPath $CliExecutable -PathType Leaf)) {
  throw "CLI executable not found: $CliExecutable. Run pnpm cli:build first."
}
if (-not (Test-Path -LiteralPath $Fixture -PathType Leaf)) {
  & (Join-Path $PSScriptRoot "generate-perf-fixture.ps1") -OutputPath $Fixture | Out-Null
}

$script:resolvedCli = (Resolve-Path -LiteralPath $CliExecutable).Path
$resolvedFixture = (Resolve-Path -LiteralPath $Fixture).Path
$resolvedDesktop = if (Test-Path -LiteralPath $DesktopExecutable -PathType Leaf) {
  (Resolve-Path -LiteralPath $DesktopExecutable).Path
} else {
  ""
}
$profileDirectory = (New-Item -ItemType Directory -Path (
  Join-Path $env:TEMP ("inkflow-cli-performance-" + [Guid]::NewGuid().ToString("N"))
)).FullName

try {
  $startupSamples = for ($index = 0; $index -lt $Iterations; $index++) {
    (Invoke-InkFlowCli -Arguments @("capabilities") -DataDirectory $profileDirectory).elapsedMs
  }
  $read = Invoke-InkFlowCli -Arguments @("document", "read", $resolvedFixture) -DataDirectory $profileDirectory
  $analysis = Invoke-InkFlowCli -Arguments @("document", "analyze", $resolvedFixture) -DataDirectory $profileDirectory

  $renderSamples = @()
  if ($resolvedDesktop) {
    $renderFixture = Join-Path $profileDirectory "renderer-ready.md"
    [System.IO.File]::WriteAllText(
      $renderFixture,
      "# Renderer readiness`n`nInline math: `$x^2`$.`n",
      [System.Text.UTF8Encoding]::new($false)
    )
    $renderOutput = Join-Path $profileDirectory "fragment.html"
    for ($index = 0; $index -lt $RenderIterations; $index++) {
      $renderSamples += (Invoke-InkFlowCli `
        -Arguments @("render", "fragment", $renderFixture, "--output", $renderOutput, "--force") `
        -DataDirectory $profileDirectory `
        -DesktopPath $resolvedDesktop).elapsedMs
    }
  }

  $startupP95 = Get-Percentile95 -Values $startupSamples
  $renderP95 = if ($renderSamples.Count -gt 0) { Get-Percentile95 -Values $renderSamples } else { $null }
  $report = [ordered]@{
    recordedAt = [DateTime]::UtcNow.ToString("o")
    cli = $script:resolvedCli
    desktop = if ($resolvedDesktop) { $resolvedDesktop } else { $null }
    fixture = $resolvedFixture
    fixtureBytes = (Get-Item -LiteralPath $resolvedFixture).Length
    iterations = $Iterations
    coldStart = [ordered]@{
      p95Ms = $startupP95
      targetMs = 150
      passed = $startupP95 -le 150
      samplesMs = $startupSamples
    }
    documentRead = [ordered]@{
      elapsedMs = $read.elapsedMs
      targetMs = 2000
      passed = $read.elapsedMs -le 2000
    }
    documentAnalyze = [ordered]@{
      elapsedMs = $analysis.elapsedMs
      targetMs = 2000
      passed = $analysis.elapsedMs -le 2000
    }
    hiddenRenderer = if ($null -ne $renderP95) {
      [ordered]@{
        p95Ms = $renderP95
        targetMs = 1500
        passed = $renderP95 -le 1500
        samplesMs = $renderSamples
      }
    } else {
      [ordered]@{ skipped = $true; reason = "Desktop release executable not found." }
    }
  }

  $resolvedOutput = [System.IO.Path]::GetFullPath($OutputPath)
  $outputParent = Split-Path -Parent $resolvedOutput
  New-Item -ItemType Directory -Path $outputParent -Force | Out-Null
  [System.IO.File]::WriteAllText(
    $resolvedOutput,
    ($report | ConvertTo-Json -Depth 8) + [Environment]::NewLine,
    [System.Text.UTF8Encoding]::new($false)
  )
  $report | ConvertTo-Json -Depth 8
  Write-Output "Saved local-only CLI performance report to $resolvedOutput"
} finally {
  $tempPrefix = [System.IO.Path]::GetFullPath($env:TEMP).TrimEnd(
    [System.IO.Path]::DirectorySeparatorChar,
    [System.IO.Path]::AltDirectorySeparatorChar
  ) + [System.IO.Path]::DirectorySeparatorChar
  $resolvedProfile = [System.IO.Path]::GetFullPath($profileDirectory)
  if (
    $resolvedProfile.StartsWith($tempPrefix, [System.StringComparison]::OrdinalIgnoreCase) -and
    (Split-Path -Leaf $resolvedProfile).StartsWith("inkflow-cli-performance-")
  ) {
    Remove-Item -LiteralPath $resolvedProfile -Recurse -Force -ErrorAction SilentlyContinue
  }
}
