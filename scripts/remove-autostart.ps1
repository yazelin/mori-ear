param(
    [string]$TaskName = "mori-ear"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$scriptDir = Split-Path -Parent $PSCommandPath
$installer = Join-Path $scriptDir "install-autostart.ps1"

if (-not (Test-Path -LiteralPath $installer -PathType Leaf)) {
    throw "Cannot find install-autostart.ps1 next to this script."
}

& $installer -Remove -TaskName $TaskName
