param([string]$WebrtcSource = (Join-Path $env:TEMP 'esp-peer-v1.3.0'))
$ErrorActionPreference = 'Stop'
$libsrtp = Join-Path $WebrtcSource 'components/esp_peer/examples/peer_demo/managed_components/espressif__esp_libsrtp'
$localLibsrtp = Join-Path $PSScriptRoot 'managed_components/espressif__esp_libsrtp'
if (Test-Path -LiteralPath $libsrtp) { $env:KVMCTL_LIBSRTP_SOURCE = $libsrtp }
elseif (Test-Path -LiteralPath $localLibsrtp) { $env:KVMCTL_LIBSRTP_SOURCE = $localLibsrtp }
else { Remove-Item Env:KVMCTL_LIBSRTP_SOURCE -ErrorAction SilentlyContinue }
$builder = Join-Path $PSScriptRoot '../verification/m0/peer-fingerprint/build.ps1'
powershell.exe -NoProfile -ExecutionPolicy Bypass -File $builder -Harden -ProjectDirectory $PSScriptRoot -WebrtcSource $WebrtcSource
exit $LASTEXITCODE
