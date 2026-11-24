param([string]$BuildDirectory = 'firmware/build/hid-m15')
$ErrorActionPreference = 'Stop'
. 'C:\Espressif\tools\Microsoft.v6.1.1.PowerShell_profile.ps1'
$env:IDF_COMPONENT_MANAGER = '0'
$env:KVMCTL_HID_BENCH = '1'
$env:KVMCTL_HID_BENCH_COMPONENTS = (Resolve-Path 'verification/m0/usb-hid-uart/managed_components').Path
$vendor = (Resolve-Path 'verification/m0/peer-fingerprint/.vendor/69797030d58208f6ffb720aa6cc0f62a001a534283dd4501dacf5a6f12f823f0/components/esp_peer').Path
$libs = (Resolve-Path 'firmware/build/windows-peer/components/esp_peer/examples/peer_demo/managed_components/espressif__esp_libsrtp').Path
$extra = @(
    $vendor,
    $libs,
    (Resolve-Path 'firmware/managed_components/espressif__led_strip').Path,
    (Resolve-Path 'firmware/managed_components/espressif__mdns').Path
) -join ';'
idf.py -C firmware -B $BuildDirectory --no-ccache -DIDF_TARGET=esp32s3 -DIDF_COMPONENT_MANAGER=0 -DEXTRA_COMPONENT_DIRS="$extra" reconfigure build
exit $LASTEXITCODE
