# Records a game's Direct3D calls with apitrace on Windows (.github/workflows/gametrace.yml):
# starts it under `apitrace trace`, lets it run SECONDS (to its title screen: nobody clicks),
# then asks its windows to close and, ten seconds later, ends whatever of it still runs. The
# trace then stops where the game did (the converter keeps its calls up to the last whole one).
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
# (quoted: Japanese names with spaces)
$a = @('trace', '--api', $Api, '-o', "`"$Out`"", "`"$Exe`"")
$at = Start-Process -FilePath $Apitrace -ArgumentList $a -WorkingDirectory $dir -PassThru -RedirectStandardError "$Out.log"
Start-Sleep -Seconds $Seconds
# the game's processes: its exe and whatever it started from its own folder (a launcher's game)
function Game { Get-Process | Where-Object { $_.Id -ne $at.Id -and $_.Path -and $_.Path.StartsWith($dir, [StringComparison]::OrdinalIgnoreCase) } }
$ran = @(Game)
Write-Host "after $Seconds s: $(@($ran | ForEach-Object { "$($_.ProcessName) ($($_.Id))" }) -join ', ')"
foreach ($p in $ran) { [void]$p.CloseMainWindow() }
Start-Sleep -Seconds 10
foreach ($p in @(Game)) { Write-Host "ending $($p.ProcessName)"; Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
if (-not $at.WaitForExit(30000)) { Stop-Process -Id $at.Id -Force -ErrorAction SilentlyContinue }
if (Test-Path -LiteralPath "$Out.log") { Get-Content -LiteralPath "$Out.log" -Tail 5 }
if (-not (Test-Path -LiteralPath $Out)) { Write-Host "no trace"; exit 1 }
Write-Host "trace $((Get-Item -LiteralPath $Out).Length) bytes"
