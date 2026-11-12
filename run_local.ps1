# RuseTect: run the API + Live Console on Windows.
# Usage (PowerShell, in the repo folder; needs Rust from https://rustup.rs):
#   .\run_local.ps1                                   # bundled 100K-payment history sample
#   .\run_local.ps1 -HistoryGlob "D:\data\aml_HI-Small_part0*.csv.gz"   # full IBM HI-Small history (benchmark-level recall)
# You can also set $env:RUSETECT_HISTORY_GLOB instead of passing -HistoryGlob.
param(
    [string]$HistoryGlob = $env:RUSETECT_HISTORY_GLOB,
    [int]$Port = 8001
)
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot
cargo build --release -p rusetect-server

if ($HistoryGlob) {
    $env:RUSETECT_HISTORY_GLOB = $HistoryGlob
    Write-Host "Account history: $HistoryGlob"
} else {
    Write-Host "No history glob set - the console uses the bundled 100K-payment sample (lower recall)."
}

$env:RUSETECT_ROOT = $PSScriptRoot
$env:RUSETECT_DEMO = "1"  # local demo: published demo keys; set RUSETECT_ANALYST_KEYS for real use
Start-Job -ScriptBlock { param($p) Start-Sleep 3; Start-Process "http://localhost:$p/" } -ArgumentList $Port | Out-Null
& .\target\release\rusetect-server.exe --host 127.0.0.1 --port $Port
