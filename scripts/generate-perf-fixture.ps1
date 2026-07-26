param(
  [string]$OutputPath = (Join-Path $env:TEMP "inkflow-100k-lines.md"),
  [int]$Lines = 100000
)

$ErrorActionPreference = "Stop"
$utf8 = [System.Text.UTF8Encoding]::new($false)
$chineseSample = -join @(
  [char]0x4E2D,
  [char]0x6587,
  [char]0x5185,
  [char]0x5BB9
)
$writer = [System.IO.StreamWriter]::new($OutputPath, $false, $utf8, 65536)
try {
  for ($index = 1; $index -le $Lines; $index++) {
    if ($index % 1000 -eq 1) {
      $writer.WriteLine("## Performance section $index")
    }
    $writer.WriteLine("Line $index - InkFlow long document benchmark $chineseSample with **Markdown** and [link](https://example.com).")
  }
} finally {
  $writer.Dispose()
}

$file = Get-Item -LiteralPath $OutputPath
[pscustomobject]@{
  path = $file.FullName
  lines = $Lines
  bytes = $file.Length
} | ConvertTo-Json
