# The PowerShell scripts of tests/ parse (a parse error shows only on the Windows runner, after
# the minutes it takes to get there): run by gametrace.yml's plan job (pwsh on ubuntu).
$bad = 0
foreach ($f in Get-ChildItem -Recurse -Path (Join-Path $PSScriptRoot '..') -Filter *.ps1) {
  $errs = $null
  [void][System.Management.Automation.Language.Parser]::ParseFile($f.FullName, [ref]$null, [ref]$errs)
  foreach ($e in $errs) { Write-Host "$($f.FullName):$($e.Extent.StartLineNumber): $($e.Message)"; $bad++ }
}
if ($bad) { exit 1 }
Write-Host "ps1-parse: ok"
