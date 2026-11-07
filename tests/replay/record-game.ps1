# Records a game's Direct3D calls with apitrace on Windows (.github/workflows/gametrace.yml):
# apitrace's wrapper DLLs for the API go beside the game's exe (it loads them as the system's;
# `apitrace trace`'s injector starts the game through the ANSI code page, and the runner's
# en-US one has no Japanese folder names), TRACE_FILE names the trace. The game runs SECONDS
# (to its title screen: nobody clicks), then its windows are asked to close and, ten seconds
# later, whatever of it still runs is ended. The trace then stops where the game did (the
# converter keeps its calls up to the last whole one).
#
#   record-game.ps1 -Apitrace <apitrace.exe> -Api d3d9|d3d8|dxgi -Exe <game.exe> -Out <x.trace> [-Seconds 60]
param(
  [Parameter(Mandatory)] [string] $Apitrace,
  [Parameter(Mandatory)] [string] $Api,
  [Parameter(Mandatory)] [string] $Exe,
  [Parameter(Mandatory)] [string] $Out,
  [int] $Seconds = 60
)
$ErrorActionPreference = 'Stop'
$Exe = (Resolve-Path -LiteralPath $Exe).Path
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
$game = Start-Process -FilePath $Exe -WorkingDirectory $dir -PassThru
Start-Sleep -Seconds $Seconds
# the game's processes: its exe and whatever it started from its own folder (a launcher's game)
function Game { Get-Process | Where-Object { $_.Path -and $_.Path.StartsWith($dir, [StringComparison]::OrdinalIgnoreCase) } }
$ran = @(Game)
Write-Host "after $Seconds s: $(@($ran | ForEach-Object { "$($_.ProcessName) ($($_.Id))" }) -join ', ') (exit $(if ($game.HasExited) { $game.ExitCode } else { 'none yet' }))"
foreach ($p in $ran) { [void]$p.CloseMainWindow() }
Start-Sleep -Seconds 10
foreach ($p in @(Game)) { Write-Host "ending $($p.ProcessName)"; Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
Start-Sleep -Seconds 2
foreach ($c in $copied) { Remove-Item -LiteralPath $c -ErrorAction SilentlyContinue }
if (-not (Test-Path -LiteralPath $Out)) { Write-Host "no trace (the game made no $Api device)"; exit 1 }
Write-Host "trace $((Get-Item -LiteralPath $Out).Length) bytes"
