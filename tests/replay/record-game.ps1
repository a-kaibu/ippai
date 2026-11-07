# Records a game's Direct3D calls with apitrace on Windows (.github/workflows/gametrace.yml):
# apitrace's wrapper DLLs for the API go beside the game's exe (it loads them as the system's;
# `apitrace trace`'s injector starts the game through the ANSI code page, and the runner's
# en-US one has no Japanese folder names), TRACE_FILE names the trace. The game is played as
# corpusref.yml plays it (tests/corpus/winplay.py play: code page 932, Return or PRE clicks
# through a launcher to the title screen, the title's steps, STORY seconds of story clicks), then
# ended. The trace stops where the game did (the converter keeps its calls up to the last whole
# one). winplay's pictures and result.json go to -Shots (what the game got to; not committed).
#
# The API gameapi.py guessed may not be the one the game draws with (a game naming d3d9 and d3d8
# draws with d3d8, or with DirectDraw): a watcher lists the graphics DLLs the game loaded, and
# when no trace was left the game is played again with the API whose system DLL it loaded
# (gameapi.py next). <Out>.api names the API of the trace; with none, <Out>.why says why
# ("ddraw: ..." when it drew with DirectDraw only, "notrace: ..." otherwise).
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
$Shots = [IO.Path]::GetFullPath($Shots)
foreach ($f in @($Out, "$Out.api", "$Out.why")) { Remove-Item -LiteralPath $f -ErrorAction SilentlyContinue }
$wrappers = Join-Path (Split-Path -Parent $Apitrace) '..\lib\wrappers'
Write-Host "wrappers: $((Get-ChildItem $wrappers -Filter *.dll).Name -join ' ')"
$gameapi = Join-Path $PSScriptRoot 'gameapi.py'
$winplay = Join-Path $PSScriptRoot '..\corpus\winplay.py'
$env:TRACE_FILE = $Out
$env:STORY = "$Story"

# (the watcher: a process of the other width shows only its own kind of modules, so a 32-bit
# game is watched from 32-bit PowerShell)
$pe = [IO.File]::ReadAllBytes($Exe)
$machine = [BitConverter]::ToUInt16($pe, [BitConverter]::ToInt32($pe, 0x3c) + 4)
$ps = if ($machine -eq 0x14c) { "$env:WINDIR\SysWOW64\WindowsPowerShell\v1.0\powershell.exe" } else { "$env:WINDIR\System32\WindowsPowerShell\v1.0\powershell.exe" }
$watchPs1 = [IO.Path]::ChangeExtension([IO.Path]::GetTempFileName(), '.ps1')
Set-Content -LiteralPath $watchPs1 -Encoding utf8 -Value @'
param($dir, $log)
$seen = @{}
while ($true) {
  foreach ($p in Get-Process | Where-Object { $_.Path -and $_.Path.StartsWith($dir, [StringComparison]::OrdinalIgnoreCase) }) {
    try { $ms = $p.Modules } catch { continue }
    foreach ($m in $ms) {
      if ($m.ModuleName -match '^(d3d\w*|ddraw|dxgi\w*|opengl32|d3dx9_\d+|dinput\w*)\.dll$' -and -not $seen[$m.FileName]) {
        $seen[$m.FileName] = 1
        Add-Content -LiteralPath $log -Encoding utf8 -Value "$($p.ProcessName): $($m.FileName)"
      }
    }
  }
  Start-Sleep -Seconds 5
}
'@

# one play of the game with the wrapper of $a beside it; returns the watcher's log
function Play([string] $a) {
  $names = @{ d3d9 = @('d3d9.dll'); d3d8 = @('d3d8.dll'); dxgi = @('dxgi.dll', 'd3d11.dll', 'dxgitrace.dll') }[$a]
  if (-not $names) { throw "api $a" }
  $copied = @()
  foreach ($n in $names) {
    $w = Join-Path $wrappers $n
    if (-not (Test-Path -LiteralPath $w)) { continue }
    $to = Join-Path $dir $n
    if (Test-Path -LiteralPath $to) { throw "the game has its own $n" }
    Copy-Item -LiteralPath $w -Destination $to
    $copied += $to
  }
  if (-not $copied) { throw "no wrapper for $a in $wrappers" }
  Write-Host "== $a: beside the game: $(($copied | ForEach-Object { Split-Path -Leaf $_ }) -join ' ')"
  $log = [IO.Path]::GetTempFileName()
  $watch = Start-Process -FilePath $ps -ArgumentList '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$watchPs1`"", "`"$dir`"", "`"$log`"" -PassThru -WindowStyle Hidden
  Remove-Item -LiteralPath $Shots -Recurse -ErrorAction SilentlyContinue
  try {
    python $winplay play $Game $Shots | Write-Host
  } finally {
    Stop-Process -Id $watch.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 2
    foreach ($c in $copied) { Remove-Item -LiteralPath $c -ErrorAction SilentlyContinue }
  }
  Write-Host "loaded:"
  Get-Content -LiteralPath $log -Encoding utf8 -ErrorAction SilentlyContinue | ForEach-Object { Write-Host "  $_" }
  $r = Join-Path $Shots 'result.json'
  if (Test-Path -LiteralPath $r) { Get-Content -LiteralPath $r -Encoding utf8 | Write-Host }
  return $log
}

$tried = @()
$a = $Api
while ($true) {
  $tried += $a
  $log = Play $a
  if (Test-Path -LiteralPath $Out) {
    Set-Content -LiteralPath "$Out.api" -Value $a -NoNewline
    Write-Host "trace ($a) $((Get-Item -LiteralPath $Out).Length) bytes"
    exit 0
  }
  $next = (python $gameapi next $log @tried | Out-String).Trim()
  Write-Host "no trace with $a; next: $(if ($next) { $next } else { 'nothing' })"
  if ($next -and $next -ne 'ddraw') { $a = $next; continue }
  $sys = @(Get-Content -LiteralPath $log -Encoding utf8 -ErrorAction SilentlyContinue | ForEach-Object { ($_ -split '\\')[-1].ToLower() } | Sort-Object -Unique) -join ' '
  $why = if ($next -eq 'ddraw') { "ddraw" } else { "notrace" }
  Set-Content -LiteralPath "$Out.why" -Value "$why`ttried $($tried -join ', '); loaded: $(if ($sys) { $sys } else { 'no graphics DLL' })" -NoNewline -Encoding utf8
  Write-Host "no trace: $why"
  exit 1
}
