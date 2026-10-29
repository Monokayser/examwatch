<#
.SYNOPSIS
  Installer smoke test (run as administrator on a throw-away machine/VM or CI).

  1. Installs ExamWatch Teacher silently; the server service must be running and
     the dashboard must answer over HTTPS with a *trusted* certificate.
  2. Creates the admin account through the first-run endpoint.
  3. Downloads "Add student PCs" ZIP, extracts it, installs the student MSI from
     that folder; the student service must run and the settings must be in HKLM.
  4. Uninstalls both; services must be gone.
#>
param([string]$Dist = "dist")
$ErrorActionPreference = 'Stop'
$Dist = (Resolve-Path $Dist).Path
$fail = 0
function Check($ok, $what) {
    if ($ok) { Write-Host "PASS  $what" -ForegroundColor Green } else { Write-Host "FAIL  $what" -ForegroundColor Red; $script:fail++ }
}
function Msi($args_) {
    $p = Start-Process msiexec.exe -ArgumentList $args_ -Wait -PassThru
    return $p.ExitCode
}
$work = Join-Path ([IO.Path]::GetTempPath()) ("ew-inst-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory $work | Out-Null

try {
    $rc = Msi @('/i', "`"$Dist\ExamWatch-Teacher-Setup.msi`"", '/qn', '/l*v', "`"$work\teacher.log`"")
    Check ($rc -eq 0) "Teacher MSI installs silently (exit $rc)"
    $svc = Get-Service ExamWatchServer -ErrorAction SilentlyContinue
    Check ($svc -and $svc.Status -eq 'Running') "ExamWatch Server service is running"
    Check (Test-Path "$env:ProgramData\ExamWatch\ew-server.toml") "server settings created in ProgramData"

    $base = 'https://localhost:8443'
    $status = $null
    for ($i = 0; $i -lt 30 -and -not $status; $i++) {
        try { $status = Invoke-RestMethod "$base/api/setup" } catch { Start-Sleep -Milliseconds 500 }
    }
    Check ($null -ne $status) "dashboard answers over HTTPS with a trusted certificate (no warning)"
    Check ($status.needs_setup -eq $true) "first run asks for an administrator account"

    $h = @{ 'X-EW' = '1' }
    $null = Invoke-RestMethod "$base/api/setup" -Method Post -Headers $h -ContentType 'application/json' `
        -Body '{"username":"admin","password":"installer-test-pw"}' -SessionVariable s
    Check ((Invoke-RestMethod "$base/api/setup").needs_setup -eq $false) "administrator created; setup closes"

    Invoke-WebRequest "$base/api/student-installer" -Headers $h -WebSession $s -OutFile "$work\student.zip" -UseBasicParsing
    Expand-Archive "$work\student.zip" "$work\student"
    Check ((Test-Path "$work\student\ExamWatch-Student.msi") -and (Test-Path "$work\student\examwatch-student.cfg")) "student ZIP has the MSI and the settings file"

    $rc = Msi @('/i', "`"$work\student\ExamWatch-Student.msi`"", '/qn', '/l*v', "`"$work\student.log`"")
    Check ($rc -eq 0) "Student MSI installs silently from the extracted folder (exit $rc)"
    $reg = Get-ItemProperty 'HKLM:\SOFTWARE\ExamWatch' -ErrorAction SilentlyContinue
    Check ($reg -and $reg.Server -match ':4433$' -and $reg.ServerPin.Length -eq 64) "student settings written to HKLM (server $($reg.Server))"
    $ss = Get-Service ExamWatchService -ErrorAction SilentlyContinue
    Check ($ss -and $ss.Status -eq 'Running') "ExamWatch Student service is running"
    Check (Test-Path "$env:PUBLIC\Desktop\V1.lnk") "V1 shortcut on the public desktop"

    # A student MSI without its settings file must refuse to install.
    New-Item -ItemType Directory "$work\nocfg" | Out-Null
    Copy-Item "$work\student\ExamWatch-Student.msi" "$work\nocfg\"
    $rc = Msi @('/x', "`"$work\student\ExamWatch-Student.msi`"", '/qn')
    $rc2 = Msi @('/i', "`"$work\nocfg\ExamWatch-Student.msi`"", '/qn', '/l*v', "`"$work\nocfg.log`"")
    Check ($rc2 -ne 0 -and -not (Get-Service ExamWatchService -ErrorAction SilentlyContinue)) "student MSI without examwatch-student.cfg rolls back cleanly"

    $rc = Msi @('/x', "`"$Dist\ExamWatch-Teacher-Setup.msi`"", '/qn')
    Check ($rc -eq 0 -and -not (Get-Service ExamWatchServer -ErrorAction SilentlyContinue)) "Teacher MSI uninstalls; service removed"
    Check (Test-Path "$env:ProgramData\ExamWatch\data\master.key") "keys and recordings kept after uninstall (until retention / manual delete)"
}
catch {
    Write-Host "FAIL  unexpected error: $_" -ForegroundColor Red
    $fail++
    foreach ($l in 'teacher.log', 'student.log') {
        if (Test-Path "$work\$l") { Write-Host "--- $l (errors) ---"; Select-String -Path "$work\$l" -Pattern 'Return value 3|Error \d{4}' -Context 3, 1 | Select-Object -First 5 }
    }
}
if ($fail) { Write-Host "`n$fail check(s) failed" -ForegroundColor Red; exit 1 }
Write-Host "`nAll installer checks passed" -ForegroundColor Green
