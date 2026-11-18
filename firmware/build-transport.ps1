param([string]$WebrtcSource = (Join-Path $env:TEMP 'esp-peer-v1.3.0'))
$ErrorActionPreference = 'Stop'
$env:KVMCTL_LIBSRTP_SOURCE = Join-Path $WebrtcSource 'components/esp_peer/examples/peer_demo/managed_components/espressif__esp_libsrtp'
if (-not (Test-Path -LiteralPath $env:KVMCTL_LIBSRTP_SOURCE)) { throw 'Cached espressif/esp_libsrtp 1.0.0 was not found in the pinned peer source.' }
$builder = Join-Path $PSScriptRoot '../verification/m0/peer-fingerprint/build.ps1'
powershell.exe -NoProfile -ExecutionPolicy Bypass -File $builder -Harden -ProjectDirectory $PSScriptRoot -WebrtcSource $WebrtcSource
exit $LASTEXITCODE
