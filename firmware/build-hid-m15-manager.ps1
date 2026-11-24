param(
    [string]$BuildDirectory = 'firmware/build/hid-m15-online',
    [string]$IdfPath = (Join-Path $env:TEMP 'kvmctl-esp-idf-v6.1.1-online')
)
$ErrorActionPreference = 'Stop'
. 'C:\Espressif\tools\Microsoft.v6.1.1.PowerShell_profile.ps1'
if (-not (Test-Path -LiteralPath (Join-Path $IdfPath 'tools/cmake/project.cmake'))) {
    throw "ESP-IDF tree is incomplete: $IdfPath"
}
$env:IDF_PATH = (Resolve-Path -LiteralPath $IdfPath).Path
$env:KVMCTL_HID_BENCH = '1'
$env:KVMCTL_HID_BENCH_COMPONENTS = (Resolve-Path 'verification/m0/usb-hid-uart/managed_components').Path
$env:KVMCTL_PEER_SOURCE = (Resolve-Path 'verification/m0/peer-fingerprint/.vendor/69797030d58208f6ffb720aa6cc0f62a001a534283dd4501dacf5a6f12f823f0/components/esp_peer').Path
$env:KVMCTL_LIBSRTP_SOURCE = (Resolve-Path 'firmware/build/windows-peer/components/esp_peer/examples/peer_demo/managed_components/espressif__esp_libsrtp').Path
$lock = (Resolve-Path 'firmware/dependencies.lock').Path
$lockBytes = [IO.File]::ReadAllBytes($lock)
$result = 1
try {
    Remove-Item -LiteralPath $lock
    idf.py -C firmware -B $BuildDirectory --no-ccache -DIDF_TARGET=esp32s3 reconfigure build
    $result = $LASTEXITCODE
} finally {
    [IO.File]::WriteAllBytes($lock, $lockBytes)
}
exit $result
