# Run on a Windows runner (.github/workflows/mfcabi.yml): writes into $args[0]
#   mfc140.tsv / mfc140u.tsv  ordinal<TAB>decorated name in the x86 import libraries
#   layout.tsv               sizes, offsets and vtable slots (layout.cpp)
#   env.txt                  the Visual Studio and MFC versions they came from
$ErrorActionPreference = 'Stop'
$out = $args[0]
New-Item -ItemType Directory -Force $out | Out-Null
$vs = & "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" -latest -products * -property installationPath
$tools = Get-Content "$vs\VC\Auxiliary\Build\Microsoft.VCToolsVersion.default.txt"
$vc = "$vs\VC\Tools\MSVC\$tools"
$bin = "$vc\bin\Hostx64\x86"
"vs=$vs`ntools=$tools" | Set-Content "$out\env.txt"
Get-ChildItem "$vc\atlmfc\lib\x86" | Select-Object -ExpandProperty Name | Add-Content "$out\env.txt"
foreach ($lib in 'mfc140', 'mfc140u') {
  $p = "$vc\atlmfc\lib\x86\$lib.lib"
  if (-not (Test-Path $p)) { "missing $p" | Add-Content "$out\env.txt"; continue }
  $sym = $null; $rows = New-Object System.Collections.Generic.List[string]
  foreach ($l in (& "$bin\dumpbin.exe" /headers $p)) {
    if ($l -match '^\s+Symbol name\s*:\s*(\S+)') { $sym = $Matches[1] }
    elseif ($l -match '^\s+Ordinal\s*:\s*(\d+)' -and $sym) {
      $rows.Add("$($Matches[1])`t$sym"); $sym = $null
    }
  }
  $rows | Sort-Object { [int]($_ -split "`t")[0] } | Set-Content -Encoding utf8 "$out\$lib.tsv"
}
Push-Location $env:RUNNER_TEMP
# (a build error goes to layout-build.txt, committed with the rest, so the probe can be fixed)
cmd /c "`"$vs\VC\Auxiliary\Build\vcvarsall.bat`" x86 >nul && cl /nologo /EHsc /MD /D_AFXDLL /std:c++17 `"$PSScriptRoot\layout.cpp`" /Fe:layout.exe 2>&1" | Set-Content -Encoding utf8 "$out\layout-build.txt"
if (Test-Path layout.exe) { .\layout.exe | Set-Content -Encoding utf8 "$out\layout.tsv" }
Pop-Location
exit 0  # (cl failing is in layout-build.txt)
