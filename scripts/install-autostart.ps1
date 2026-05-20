param(
    [string]$ExePath,
    [switch]$Remove,
    [string]$TaskName = "mori-ear",
    [int]$DelaySeconds = 5,
    # 內部用:self-elevate path 把原本呼叫者的 identity 傳進 elevated 那層,
    # 避免 UAC 把 elevated shell 跑在另一個 admin 帳號下、結果 task 註冊給錯人。
    [string]$OriginalUser
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

# Register-ScheduledTask / Unregister-ScheduledTask 在 `\` root 一定要 admin。
# 非 admin 跑會 silent fail 在「Access is denied」(line 79 的 Register 那行)。
# 偵測到沒 elevation → Start-Process -Verb RunAs 重啟自己;原本參數逐字傳過去,
# 並把 caller 的 identity 透過 -OriginalUser 帶進去(下面 Get-CurrentUser 用)。
function Test-IsAdmin {
    $id = [System.Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object System.Security.Principal.WindowsPrincipal($id)
    return $principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)
}

if (-not (Test-IsAdmin)) {
    # 先在 non-admin context 把 exe 路徑 resolve 成絕對路徑 —— elevated shell 的
    # 工作目錄、PATH 可能不同(尤其 UAC 跨帳號時),`.\mori-ear.exe` 跟
    # `Get-Command mori-ear.exe` 在那邊的結果跟現在不一樣。
    $resolvedExe = $null
    if ($ExePath) {
        try { $resolvedExe = (Resolve-Path -LiteralPath $ExePath -ErrorAction Stop).Path } catch {}
    } else {
        $scriptDir = Split-Path -Parent $PSCommandPath
        $nearScript = Join-Path $scriptDir "mori-ear.exe"
        if (Test-Path -LiteralPath $nearScript -PathType Leaf) {
            $resolvedExe = (Resolve-Path -LiteralPath $nearScript).Path
        } else {
            $fromPath = Get-Command "mori-ear.exe" -ErrorAction SilentlyContinue
            if ($fromPath -and $fromPath.Source) {
                $resolvedExe = $fromPath.Source
            }
        }
    }

    $originalUser = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name

    $argList = @(
        "-ExecutionPolicy", "Bypass",
        "-NoProfile",
        "-File", $PSCommandPath,
        "-OriginalUser", $originalUser
    )
    if ($resolvedExe)             { $argList += @("-ExePath", $resolvedExe) }
    if ($Remove)                  { $argList += "-Remove" }
    if ($TaskName -ne "mori-ear") { $argList += @("-TaskName", $TaskName) }
    if ($DelaySeconds -ne 5)      { $argList += @("-DelaySeconds", "$DelaySeconds") }

    $verb = if ($Remove) { "移除" } else { "註冊" }
    Write-Host "$verb scheduled task 需要 admin,跳 UAC 提示中..." -ForegroundColor Yellow

    try {
        $p = Start-Process powershell `
            -ArgumentList $argList `
            -Verb RunAs `
            -WindowStyle Hidden `
            -Wait -PassThru `
            -ErrorAction Stop
    } catch {
        Write-Host "UAC 被取消 / elevation 失敗 — scheduled task 沒動。" -ForegroundColor Red
        Write-Host "改從 admin PowerShell 跑,或在跳 UAC 時點「是」。" -ForegroundColor Red
        exit 1
    }

    if ($p.ExitCode -eq 0) {
        if ($Remove) {
            Write-Host "✓ Removed scheduled task: $TaskName" -ForegroundColor Green
        } else {
            Write-Host "✓ Installed scheduled task: $TaskName" -ForegroundColor Green
            if ($resolvedExe) { Write-Host "  Executable: $resolvedExe" }
            Write-Host "  下次登入會自動啟動(delay ${DelaySeconds}s)。"
            Write-Host "  移除:powershell -ExecutionPolicy Bypass -File .\remove-autostart.ps1"
        }
    } else {
        Write-Host "Elevated install/remove failed with exit code $($p.ExitCode)" -ForegroundColor Red
    }
    exit $p.ExitCode
}

# ===== 以下都在 elevated context =====

function Get-CurrentUser {
    # 走 self-elevate path 進來時 $OriginalUser 是 caller 身份(本機帳號用戶),
    # 不是當前 elevated shell 跑的帳號 — 確保 task 是替原本那個人裝。
    if ($OriginalUser) { return $OriginalUser }
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
