<#
  Registers RustSolar to start automatically at Windows startup as a Scheduled Task that
  runs whether or not anyone is signed in, restarts on failure, and has no time limit.

  Run from an elevated PowerShell:
      .\deploy\install-windows-task.ps1 -Exe "C:\Program Files\RustSolar\rustsolar.exe"

  The task runs as the current user so it can read the token file under
  %LOCALAPPDATA%\RustSolar. Remove with:  Unregister-ScheduledTask -TaskName RustSolar
#>
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [string]$DataDir = "$env:LOCALAPPDATA\RustSolar"
)
$ErrorActionPreference = 'Stop'
if (-not (Test-Path $Exe)) { throw "Not found: $Exe" }
New-Item -ItemType Directory -Force $DataDir | Out-Null

$action = New-ScheduledTaskAction -Execute $Exe -Argument "--data-dir `"$DataDir`" serve" -WorkingDirectory (Split-Path $Exe)
$trigger = New-ScheduledTaskTrigger -AtStartup
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) -ExecutionTimeLimit ([TimeSpan]::Zero) -StartWhenAvailable
$cred = Get-Credential -UserName "$env:USERDOMAIN\$env:USERNAME" -Message "Windows password for the account that will run RustSolar"
Register-ScheduledTask -TaskName 'RustSolar' -Description 'RustSolar - Enphase Energy Intelligence' `
    -Action $action -Trigger $trigger -Settings $settings `
    -User $cred.UserName -Password $cred.GetNetworkCredential().Password -RunLevel Limited -Force | Out-Null
Start-ScheduledTask -TaskName 'RustSolar'
Write-Host "RustSolar registered and started. Dashboard: http://127.0.0.1:8080"
