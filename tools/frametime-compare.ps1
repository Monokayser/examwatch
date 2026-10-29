<#
.SYNOPSIS
  Compares two PresentMon CSV captures (ExamWatch off vs on) against the
  design §2.8 budget: p99 frame time may grow by at most 0.5 ms, and there
  must be no extra dropped frames.

.EXAMPLE
  .\tools\frametime-compare.ps1 off.csv on.csv
#>
param(
    [Parameter(Mandatory)][string]$Off,
    [Parameter(Mandatory)][string]$On,
    [double]$MaxP99IncreaseMs = 0.5
)
$ErrorActionPreference = 'Stop'

function Stats($path) {
    $rows = Import-Csv $path
    if (-not $rows) { throw "$path has no rows (was the right process captured?)" }
    # PresentMon 1.x writes MsBetweenPresents; 2.x writes FrameTime (and may keep the old name).
    $col = @('FrameTime', 'MsBetweenPresents', 'msBetweenPresents') | Where-Object { $rows[0].PSObject.Properties.Name -contains $_ } | Select-Object -First 1
    if (-not $col) { throw "$path has no FrameTime/MsBetweenPresents column" }
    $t = $rows | ForEach-Object { [double]$_.$col } | Where-Object { $_ -gt 0 } | Sort-Object
    $pct = { param($p) $t[[Math]::Min($t.Count - 1, [int][Math]::Floor($p * ($t.Count - 1)))] }
    $dropped = 0
    if ($rows[0].PSObject.Properties.Name -contains 'Dropped') { $dropped = @($rows | Where-Object { $_.Dropped -eq '1' }).Count }
    [pscustomobject]@{
        File    = Split-Path $path -Leaf
        Frames  = $t.Count
        AvgMs   = [Math]::Round(($t | Measure-Object -Average).Average, 3)
        P50Ms   = [Math]::Round((& $pct 0.50), 3)
        P99Ms   = [Math]::Round((& $pct 0.99), 3)
        MaxMs   = [Math]::Round($t[-1], 3)
        Dropped = $dropped
    }
}

$a = Stats $Off
$b = Stats $On
$a, $b | Format-Table -AutoSize

$dP99 = [Math]::Round($b.P99Ms - $a.P99Ms, 3)
$dDrop = $b.Dropped - $a.Dropped
Write-Host ("p99 change: {0:+0.000;-0.000} ms (limit +{1} ms)" -f $dP99, $MaxP99IncreaseMs)
Write-Host ("dropped-frame change: {0:+0;-0;0}" -f $dDrop)
if ($dP99 -le $MaxP99IncreaseMs -and $dDrop -le 0) {
    Write-Host "PASS  ExamWatch made no measurable difference to frame pacing" -ForegroundColor Green
} else {
    Write-Host "FAIL  frame pacing got worse with ExamWatch capturing" -ForegroundColor Red
    exit 1
}
