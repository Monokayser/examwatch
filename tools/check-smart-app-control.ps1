<#
.SYNOPSIS
  Reports whether Windows Smart App Control (SAC) is on, which would block
  unsigned ExamWatch programs. Read-only: changes nothing.

.EXAMPLE
  # This PC:
  .\check-smart-app-control.ps1

.EXAMPLE
  # Many lab PCs (needs WinRM / PowerShell remoting, normal on domain PCs):
  .\check-smart-app-control.ps1 -ComputerName (Get-Content .\lab-pcs.txt) | Export-Csv sac.csv -NoTypeInformation

.NOTES
  VerifiedAndReputablePolicyState: 0 = Off, 1 = On (enforce), 2 = Evaluation.
  "Evaluation" PCs can switch themselves to On later, so treat them like On.
  Windows 10 has no Smart App Control: those PCs report "Not present".
#>
param([string[]]$ComputerName = @($env:COMPUTERNAME))

$probe = {
    $os = Get-CimInstance Win32_OperatingSystem
    $state = $null
    try { $state = (Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\CI\Policy' -ErrorAction Stop).VerifiedAndReputablePolicyState } catch { }
    $sac = switch ($state) {
        0       { 'Off' }
        1       { 'On' }
        2       { 'Evaluation' }
        default { 'Not present' }
    }
    $domain = (Get-CimInstance Win32_ComputerSystem).PartOfDomain
    [pscustomobject]@{
        Computer          = $env:COMPUTERNAME
        Windows           = $os.Caption
        Build             = $os.BuildNumber
        DomainJoined      = $domain
        SmartAppControl   = $sac
        UnsignedAgentOK   = ($sac -eq 'Off' -or $sac -eq 'Not present')
    }
}

foreach ($c in $ComputerName) {
    try {
        if ($c -eq $env:COMPUTERNAME -or $c -eq 'localhost' -or $c -eq '.') { & $probe }
        else { Invoke-Command -ComputerName $c -ScriptBlock $probe -ErrorAction Stop | Select-Object * -ExcludeProperty PSComputerName, RunspaceId, PSShowComputerName }
    } catch {
        [pscustomobject]@{ Computer = $c; Windows = ''; Build = ''; DomainJoined = ''; SmartAppControl = "Unreachable: $($_.Exception.Message)"; UnsignedAgentOK = $null }
    }
}
