param(
  [string]$Executable = (Join-Path $PSScriptRoot "..\src-tauri\target\release\inkflow.exe"),
  [string]$Fixture = (Join-Path $env:TEMP "inkflow-100k-lines.md"),
  [int]$TimeoutSeconds = 15,
  [int]$IdleTimeoutSeconds = 8
)

$ErrorActionPreference = "Stop"

function Get-ProcessTreeSnapshot {
  param([int]$RootProcessId)

  try {
    $processRows = @(Get-CimInstance Win32_Process -ErrorAction Stop | Select-Object ProcessId, ParentProcessId)
  } catch {
    throw "Unable to enumerate the InkFlow/WebView2 process tree: $($_.Exception.Message)"
  }
  $processIds = [System.Collections.Generic.HashSet[int]]::new()
  $pendingIds = [System.Collections.Generic.Queue[int]]::new()
  $processIds.Add($RootProcessId) | Out-Null
  $pendingIds.Enqueue($RootProcessId)
  while ($pendingIds.Count -gt 0) {
    $parentId = $pendingIds.Dequeue()
    foreach ($row in $processRows) {
      $childId = [int]$row.ProcessId
      if ([int]$row.ParentProcessId -eq $parentId -and $processIds.Add($childId)) {
        $pendingIds.Enqueue($childId)
      }
    }
  }

  $workingSetBytes = [long]0
  $totalCpuMs = 0.0
  $processCount = 0
  foreach ($processId in $processIds) {
    try {
      $item = Get-Process -Id $processId -ErrorAction Stop
      $workingSetBytes += $item.WorkingSet64
      $totalCpuMs += $item.TotalProcessorTime.TotalMilliseconds
      $processCount += 1
    } catch {
      # Child processes may exit while the snapshot is being collected.
    }
  }
  [pscustomobject]@{
    workingSetBytes = $workingSetBytes
    totalCpuMs = $totalCpuMs
    processCount = $processCount
  }
}

function Wait-ForProcessTreeIdle {
  param(
    [int]$RootProcessId,
    [int]$TimeoutSeconds
  )

  $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
  $previous = Get-ProcessTreeSnapshot -RootProcessId $RootProcessId
  $stableSamples = 0
  do {
    Start-Sleep -Milliseconds 300
    $current = Get-ProcessTreeSnapshot -RootProcessId $RootProcessId
    $cpuDeltaMs = [Math]::Max(0, $current.totalCpuMs - $previous.totalCpuMs)
    $workingSetDelta = [Math]::Abs($current.workingSetBytes - $previous.workingSetBytes)
    if ($cpuDeltaMs -le 30 -and $workingSetDelta -le 1MB) {
      $stableSamples += 1
    } else {
      $stableSamples = 0
    }
    if ($stableSamples -ge 4) {
      return [pscustomobject]@{ snapshot = $current; settled = $true }
    }
    $previous = $current
  } while ([DateTime]::UtcNow -lt $deadline)

  [pscustomobject]@{ snapshot = $previous; settled = $false }
}

if (-not (Test-Path -LiteralPath $Fixture)) {
  & (Join-Path $PSScriptRoot "generate-perf-fixture.ps1") -OutputPath $Fixture | Out-Null
}
if (-not (Test-Path -LiteralPath $Executable)) {
  throw "Release executable not found: $Executable. Run pnpm tauri build --no-bundle first."
}

$resolvedExecutable = (Resolve-Path -LiteralPath $Executable).Path
$resolvedFixture = (Resolve-Path -LiteralPath $Fixture).Path
$fixtureInfo = Get-Item -LiteralPath $resolvedFixture
$fixtureArgument = '"' + $resolvedFixture + '"'
$processName = [System.IO.Path]::GetFileNameWithoutExtension($resolvedExecutable)
$existing = @(Get-Process -Name $processName -ErrorAction SilentlyContinue)
if ($existing.Count -gt 0) {
  throw "Close the existing InkFlow test executable before running the performance pass."
}

$profileDirectory = (New-Item -ItemType Directory -Path (
  Join-Path $env:TEMP ("inkflow-performance-" + [Guid]::NewGuid().ToString("N"))
)).FullName
$tempRoot = [System.IO.Path]::GetFullPath($env:TEMP).TrimEnd([System.IO.Path]::DirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
$profileName = [System.IO.Path]::GetFileName($profileDirectory)
$profileIsDisposable = $profileDirectory.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -and
  $profileName.StartsWith("inkflow-performance-", [StringComparison]::Ordinal)
if (-not $profileIsDisposable) {
  throw "Refusing to use an unexpected performance profile directory: $profileDirectory"
}
$profileVariable = "INKFLOW_PERFORMANCE_PROFILE"
$previousProfile = [Environment]::GetEnvironmentVariable($profileVariable, "Process")
$readyMarker = Join-Path $profileDirectory "performance-ready"
$process = $null
$stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
$windowCreatedMs = $null
$interactiveReadyMs = $null
try {
  try {
    [Environment]::SetEnvironmentVariable($profileVariable, $profileDirectory, "Process")
    $process = Start-Process -FilePath $resolvedExecutable -ArgumentList @($fixtureArgument) -PassThru
  } finally {
    [Environment]::SetEnvironmentVariable($profileVariable, $previousProfile, "Process")
  }
  $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
  while ([DateTime]::UtcNow -lt $deadline) {
    Start-Sleep -Milliseconds 25
    $process.Refresh()
    if ($process.HasExited) { throw "InkFlow exited before its main window became ready." }
    if ($null -eq $windowCreatedMs -and $process.MainWindowHandle -ne 0) {
      $windowCreatedMs = $stopwatch.Elapsed.TotalMilliseconds
    }
    if (
      $null -ne $windowCreatedMs -and
      (Test-Path -LiteralPath $readyMarker) -and
      $process.MainWindowTitle.IndexOf($fixtureInfo.Name, [StringComparison]::OrdinalIgnoreCase) -ge 0
    ) {
      $interactiveReadyMs = $stopwatch.Elapsed.TotalMilliseconds
      break
    }
  }
  if ($null -eq $windowCreatedMs) { throw "InkFlow did not create a main window within $TimeoutSeconds seconds." }
  if ($null -eq $interactiveReadyMs) { throw "InkFlow did not report an interactive editor within $TimeoutSeconds seconds." }

  $idle = Wait-ForProcessTreeIdle -RootProcessId $process.Id -TimeoutSeconds $IdleTimeoutSeconds
  $process.Refresh()
  $windowTitle = $process.MainWindowTitle
  $result = [pscustomobject]@{
    measuredAt = [DateTime]::UtcNow.ToString("o")
    executable = (Resolve-Path -LiteralPath $Executable).Path
    fixture = $fixtureInfo.FullName
    fixtureBytes = $fixtureInfo.Length
    windowTitle = $windowTitle
    windowCreatedMs = [Math]::Round($windowCreatedMs, 1)
    interactiveReadyMs = [Math]::Round($interactiveReadyMs, 1)
    idleWorkingSetMb = [Math]::Round($idle.snapshot.workingSetBytes / 1MB, 1)
    idleProcessCount = $idle.snapshot.processCount
    idleSettled = $idle.settled
    note = "Interactive readiness comes from the mounted editor marker; working set includes the InkFlow/WebView2 process tree. Input p95 and scroll FPS are collected during the Windows interaction pass."
  }
  $outputDirectory = Join-Path $PSScriptRoot "..\release"
  New-Item -ItemType Directory -Force -Path $outputDirectory | Out-Null
  $output = Join-Path $outputDirectory "performance-windows.json"
  $result | ConvertTo-Json | Set-Content -LiteralPath $output -Encoding utf8
  $result | ConvertTo-Json
} finally {
  if ($null -ne $process -and -not $process.HasExited) {
    Stop-Process -Id $process.Id -Force
    Wait-Process -Id $process.Id -ErrorAction SilentlyContinue
  }
  if ($profileIsDisposable) {
    Remove-Item -LiteralPath $profileDirectory -Recurse -Force -ErrorAction SilentlyContinue
  }
}
