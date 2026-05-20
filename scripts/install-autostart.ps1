param(
    [string]$ExePath,
    [switch]$Remove,
    [string]$TaskName = "mori-ear",
    [int]$DelaySeconds = 5
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Get-CurrentUser {
    return [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
}

function Resolve-MoriEarExe {
    param([string]$PathFromUser)

    if ($PathFromUser) {
        return (Resolve-Path -LiteralPath $PathFromUser).Path
    }

    $scriptDir = Split-Path -Parent $PSCommandPath
    $nearScript = Join-Path $scriptDir "mori-ear.exe"
    if (Test-Path -LiteralPath $nearScript -PathType Leaf) {
        return (Resolve-Path -LiteralPath $nearScript).Path
    }

    $fromPath = Get-Command "mori-ear.exe" -ErrorAction SilentlyContinue
    if ($fromPath -and $fromPath.Source) {
        return $fromPath.Source
    }

    throw "Cannot find mori-ear.exe. Pass -ExePath C:\path\to\mori-ear.exe or place this script next to mori-ear.exe."
}

function Remove-MoriEarTask {
    param([string]$Name)

    $task = Get-ScheduledTask -TaskPath "\" -TaskName $Name -ErrorAction SilentlyContinue
    if ($task) {
        Unregister-ScheduledTask -TaskPath "\" -TaskName $Name -Confirm:$false
        Write-Host "Removed scheduled task: $Name"
    } else {
        Write-Host "Scheduled task not found: $Name"
    }
}

if ($Remove) {
    Remove-MoriEarTask -Name $TaskName
    return
}

$resolvedExe = Resolve-MoriEarExe -PathFromUser $ExePath
if (-not (Test-Path -LiteralPath $resolvedExe -PathType Leaf)) {
    throw "mori-ear.exe not found: $resolvedExe"
}

# 直接呼叫 mori-ear.exe —— release build 設了 windows_subsystem = "windows",
# 沒 console 視窗,不會跳黑框。
# (舊版包 powershell.exe -WindowStyle Hidden 是為了藏 console;但 Hidden 只對 GUI
#  視窗有效,對 console subsystem 的子程序沒用,所以還是會閃黑框 — 改 binary 才是根治。)
$action = New-ScheduledTaskAction -Execute $resolvedExe

$trigger = New-ScheduledTaskTrigger -AtLogOn
if ($DelaySeconds -gt 0) {
    $trigger.Delay = "PT${DelaySeconds}S"
}

$principal = New-ScheduledTaskPrincipal `
    -UserId (Get-CurrentUser) `
    -LogonType Interactive `
    -RunLevel Limited

$settings = New-ScheduledTaskSettingsSet `
    -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries `
    -MultipleInstances IgnoreNew

Register-ScheduledTask `
    -TaskPath "\" `
    -TaskName $TaskName `
    -Action $action `
    -Trigger $trigger `
    -Principal $principal `
    -Settings $settings `
    -Description "Start mori-ear at Windows logon." `
    -Force | Out-Null

Write-Host "Installed scheduled task: $TaskName"
Write-Host "Executable: $resolvedExe"
Write-Host "It will start after logon."
Write-Host "Remove with: powershell -ExecutionPolicy Bypass -File .\install-autostart.ps1 -Remove"
