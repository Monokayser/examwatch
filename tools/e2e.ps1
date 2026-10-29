<#
.SYNOPSIS
  End-to-end test: real ew-server + simulated student PCs + teacher API.

  Checks: agents connect and land in the right lab; exam opens; students
  "press V1"; a duplicate Student ID is rejected; recordings are written,
  decrypt, start on a keyframe and cover the run; closing the exam works;
  the audit log records it all.

.EXAMPLE
  .\tools\e2e.ps1 -Bin target\release -PcsPerLab 5 -Seconds 60
#>
param(
    [string]$Bin = "target\release",
    [int]$PcsPerLab = 5,
    [int]$Seconds = 60
)
$ErrorActionPreference = 'Stop'
$Bin = (Resolve-Path $Bin).Path
$work = Join-Path ([IO.Path]::GetTempPath()) ("ew-e2e-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory $work | Out-Null
$token = -join ((1..40) | ForEach-Object { '{0:x}' -f (Get-Random -Max 16) })
$cfg = Join-Path $work 'ew-server.toml'
$dataDir = (Join-Path $work 'data') -replace '\\', '/'
@"
agent_listen = "127.0.0.1:14433"
http_listen = "127.0.0.1:18443"
http_tls = false
data_dir = "$dataDir"
enroll_token = "$token"
retention_hours = 48
segment_secs = 20

[[labs]]
id = "anim1"
name = "3D Animation Lab 1"
pc_prefixes = ["A1-"]
expected_pcs = $PcsPerLab

[[labs]]
id = "visarts"
name = "Visual Arts & Communication"
pc_prefixes = ["VA-"]
expected_pcs = $PcsPerLab
"@ | Set-Content -Encoding utf8 $cfg

$fail = 0
function Check($ok, $what) {
    if ($ok) { Write-Host "PASS  $what" -ForegroundColor Green }
    else { Write-Host "FAIL  $what" -ForegroundColor Red; $script:fail++ }
}

$server = $null; $sim = $null
try {
    $env:EW_PASSWORD = 'e2e-password-123'
    & "$Bin\ew-server.exe" --config $cfg add-teacher admin --admin | Out-Null
    Check ($LASTEXITCODE -eq 0) "teacher account created"

    $server = Start-Process "$Bin\ew-server.exe" -ArgumentList '--config', $cfg, 'run' -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput "$work\server.out" -RedirectStandardError "$work\server.err"
    $pinFile = Join-Path $work 'data\server.fingerprint.txt'
    for ($i = 0; $i -lt 60 -and -not (Test-Path $pinFile); $i++) { Start-Sleep -Milliseconds 500 }
    Check (Test-Path $pinFile) "server started and wrote its certificate pin"
    $pin = (Get-Content $pinFile -Raw).Trim()

    $base = 'http://127.0.0.1:18443'
    $h = @{ 'X-EW' = '1' }
    $bad = $null
    try { Invoke-RestMethod "$base/api/login" -Method Post -Headers $h -ContentType 'application/json' -Body '{"username":"admin","password":"wrong-password"}' | Out-Null } catch { $bad = $_.Exception.Response.StatusCode.value__ }
    Check ($bad -eq 401) "wrong password rejected (401)"
    $null = Invoke-RestMethod "$base/api/login" -Method Post -Headers $h -ContentType 'application/json' -Body '{"username":"admin","password":"e2e-password-123"}' -SessionVariable s
    Check ($true) "teacher login"
    $unauth = $null
    try { Invoke-RestMethod "$base/api/exams/history" | Out-Null } catch { $unauth = $_.Exception.Response.StatusCode.value__ }
    Check ($unauth -eq 401) "API refuses requests without a session"

    $dupEvery = $PcsPerLab + 2   # one PC types an ID that's already in use
    $sim = Start-Process "$Bin\ew-sim.exe" -PassThru -WindowStyle Hidden `
        -ArgumentList '--server', '127.0.0.1:14433', '--pin', $pin, '--token', $token, '--labs', "A1-:$PcsPerLab,VA-:$PcsPerLab", '--rec', '640x360', '--dup-every', $dupEvery `
        -RedirectStandardOutput "$work\sim.out" -RedirectStandardError "$work\sim.err"
    Start-Sleep -Seconds 8

    $body = @{ name = 'E2E exam'; labs = @('anim1', 'visarts'); roster = '' } | ConvertTo-Json
    $exam = Invoke-RestMethod "$base/api/exams" -Method Post -Headers $h -WebSession $s -ContentType 'application/json' -Body $body
    Check ($exam.id -gt 0) "exam opened (id $($exam.id))"
    $dupErr = $null
    try { Invoke-RestMethod "$base/api/exams" -Method Post -Headers $h -WebSession $s -ContentType 'application/json' -Body $body | Out-Null } catch { $dupErr = $_.Exception.Response.StatusCode.value__ }
    Check ($dupErr -eq 400) "second exam on the same lab refused"

    Start-Sleep -Seconds $Seconds

    $rec = Invoke-RestMethod "$base/api/exams/$($exam.id)/recordings" -Headers $h -WebSession $s
    $expected = 2 * $PcsPerLab - 1
    Check ($rec.students.Count -eq $expected) "recordings for $($rec.students.Count) students (expected $expected: one duplicate ID blocked)"
    $labs = $rec.students | Group-Object lab_id
    Check (($labs | Where-Object Name -eq 'anim1').Count -ge 1 -and ($labs | Where-Object Name -eq 'visarts').Count -ge 1) "students assigned to both labs"
    $span = ($rec.students | ForEach-Object { ($_.end_us - $_.start_us) / 1e6 } | Measure-Object -Minimum).Minimum
    Check ($span -ge ($Seconds * 0.6)) "every recording covers most of the run (shortest $([int]$span)s of $Seconds s)"

    $first = $rec.students[0].student_id
    $segs = Invoke-RestMethod "$base/api/exams/$($exam.id)/students/$first/segments" -Headers $h -WebSession $s
    Check ($segs.segments.Count -ge 2) "recording rotated into $($segs.segments.Count) segments"
    $r = Invoke-WebRequest "$base/api/segments/$($segs.segments[0].id)" -Headers $h -WebSession $s -UseBasicParsing
    $bytes = $r.Content
    if ($bytes -is [string]) { $bytes = [Text.Encoding]::GetEncoding(28591).GetBytes($bytes) }
    Check ($bytes.Length -gt 1000 -and ($bytes[0] -band 1) -eq 1) "segment decrypts and starts with a keyframe ($($bytes.Length) bytes)"

    $audit = Invoke-RestMethod "$base/api/audit" -Headers $h -WebSession $s
    Check (@($audit.entries | Where-Object action -eq 'claim_rejected').Count -ge 1) "duplicate ID attempt is in the audit log"
    Check (@($audit.entries | Where-Object action -eq 'view_recording').Count -ge 1) "viewing a recording is audited"

    $null = Invoke-RestMethod "$base/api/exams/$($exam.id)/close" -Method Post -Headers $h -WebSession $s
    $hist = Invoke-RestMethod "$base/api/exams/history" -Headers $h -WebSession $s
    Check ($null -ne ($hist.exams | Where-Object id -eq $exam.id).closed_us) "exam closed"

    Check (-not $server.HasExited) "server still running after the whole test"
}
catch {
    Write-Host "FAIL  unexpected error: $_" -ForegroundColor Red
    $fail++
}
finally {
    foreach ($p in @($sim, $server)) { if ($p -and -not $p.HasExited) { Stop-Process -Id $p.Id -Force } }
    Write-Host "`n--- simulator (last lines) ---"; Get-Content "$work\sim.out" -Tail 6 -ErrorAction SilentlyContinue
    Write-Host "--- server errors ---"; Get-Content "$work\server.out", "$work\server.err" -ErrorAction SilentlyContinue | Select-String -Pattern 'ERROR|panicked' | Select-Object -First 20
}
if ($fail) { Write-Host "`n$fail check(s) failed" -ForegroundColor Red; exit 1 }
Write-Host "`nAll end-to-end checks passed" -ForegroundColor Green
