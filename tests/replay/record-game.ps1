# Records a game's Direct3D calls with apitrace on Windows (.github/workflows/gametrace.yml):
# apitrace's wrapper DLLs for the API go beside the game's exe (it loads them as the system's;
# `apitrace trace`'s injector starts the game through the ANSI code page, and the runner's
# en-US one has no Japanese folder names), TRACE_FILE names the trace. The game is played as
# corpusref.yml plays it (tests/corpus/winplay.py play: code page 932, Return or PRE clicks
# through a launcher to the title screen, the title's steps, STORY seconds of story clicks), then
# ended. The trace stops where the game did (the converter keeps its calls up to the last whole
# one). winplay's pictures and result.json go to -Shots (what the game got to; not committed).
#
#   record-game.ps1 -Apitrace <apitrace.exe> -Api d3d9|d3d8|dxgi -Game <dir: meta.json, x/> -Out <x.trace> -Shots <dir> [-Story 30]
param(
  [Parameter(Mandatory)] [string] $Apitrace,
  [Parameter(Mandatory)] [string] $Api,
  [Parameter(Mandatory)] [string] $Game,
  [Parameter(Mandatory)] [string] $Out,
  [Parameter(Mandatory)] [string] $Shots,
  [int] $Story = 30
)
$ErrorActionPreference = 'Stop'
$m = Get-Content -LiteralPath (Join-Path $Game 'meta.json') -Encoding utf8 | ConvertFrom-Json
$Exe = (Resolve-Path -LiteralPath (Join-Path (Join-Path $Game 'x') $m.exe)).Path
$dir = Split-Path -Parent $Exe
$Out = [IO.Path]::GetFullPath($Out)
Remove-Item -LiteralPath $Out -ErrorAction SilentlyContinue
$wrappers = Join-Path (Split-Path -Parent $Apitrace) '..\lib\wrappers'
Write-Host "wrappers: $((Get-ChildItem $wrappers -Filter *.dll).Name -join ' ')"
$names = @{ d3d9 = @('d3d9.dll'); d3d8 = @('d3d8.dll'); dxgi = @('dxgi.dll', 'd3d11.dll', 'dxgitrace.dll') }[$Api]
if (-not $names) { throw "api $Api" }
$copied = @()
foreach ($n in $names) {
  $w = Join-Path $wrappers $n
  if (-not (Test-Path -LiteralPath $w)) { continue }
  $to = Join-Path $dir $n
  if (Test-Path -LiteralPath $to) { throw "the game has its own $n" }
  Copy-Item -LiteralPath $w -Destination $to
  $copied += $to
}
if (-not $copied) { throw "no wrapper for $Api in $wrappers" }
Write-Host "beside the game: $(($copied | ForEach-Object { Split-Path -Leaf $_ }) -join ' ')"
$env:TRACE_FILE = $Out
$env:STORY = "$Story"
try {
  python (Join-Path $PSScriptRoot '..\corpus\winplay.py') play $Game $Shots
} finally {
  Start-Sleep -Seconds 2
  foreach ($c in $copied) { Remove-Item -LiteralPath $c -ErrorAction SilentlyContinue }
}
$r = Join-Path $Shots 'result.json'
if (Test-Path -LiteralPath $r) { Get-Content -LiteralPath $r -Encoding utf8 }
if (-not (Test-Path -LiteralPath $Out)) { Write-Host "no trace (the game made no $Api device)"; exit 1 }
Write-Host "trace $((Get-Item -LiteralPath $Out).Length) bytes"
