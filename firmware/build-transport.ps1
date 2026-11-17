param([string]$WebrtcSource = (Join-Path $env:TEMP 'esp-peer-v1.3.0'))
$ErrorActionPreference = 'Stop'
$builder = Join-Path $PSScriptRoot '../verification/m0/peer-fingerprint/build.ps1'
powershell.exe -NoProfile -ExecutionPolicy Bypass -File $builder -Harden -ProjectDirectory $PSScriptRoot -WebrtcSource $WebrtcSource
exit $LASTEXITCODE
