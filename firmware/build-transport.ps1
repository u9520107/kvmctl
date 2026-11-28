param(
    [string]$WebrtcSource = (Join-Path $env:TEMP 'esp-peer-v1.3.0'),
    [string]$BuildDirectory = (Join-Path $PSScriptRoot 'build/windows-production')
)

$ErrorActionPreference = 'Stop'
$profile = 'C:\Espressif\tools\Microsoft.v6.1.1.PowerShell_profile.ps1'
if (-not (Test-Path -LiteralPath $profile)) {
    throw "Missing pinned ESP-IDF 6.1.1 PowerShell profile: $profile"
}
. $profile
$env:PYTHONUTF8 = '1'

$idfVersion = (& idf.py --version 2>&1 | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or $idfVersion -notmatch '^ESP-IDF v6\.1\.1$') {
    throw "Expected ESP-IDF v6.1.1; got '$idfVersion'"
}

$project = (Resolve-Path -LiteralPath $PSScriptRoot).Path
$peer = Join-Path $project 'third_party/esp_peer'
if (-not (Test-Path -LiteralPath (Join-Path $peer 'CMakeLists.txt'))) {
    throw "Checked-in JetKVM Mini peer source is missing: $peer"
}

$libCandidates = @()
if ($env:KVMCTL_LIBSRTP_SOURCE) { $libCandidates += $env:KVMCTL_LIBSRTP_SOURCE }
$libCandidates += Join-Path $project 'managed_components/espressif__esp_libsrtp'
$libCandidates += Join-Path $WebrtcSource 'components/esp_peer/examples/peer_demo/managed_components/espressif__esp_libsrtp'
$lib = $libCandidates | Where-Object { Test-Path -LiteralPath (Join-Path $_ 'CMakeLists.txt') } | Select-Object -First 1
if (-not $lib) {
    throw 'Missing pinned esp_libsrtp 1.0.0 component. Set KVMCTL_LIBSRTP_SOURCE or provide the ESP WebRTC checkout used for its managed component.'
}

$buildRoot = [IO.Path]::GetFullPath((Join-Path $project 'build'))
$build = [IO.Path]::GetFullPath($BuildDirectory)
if (-not $build.StartsWith($buildRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -and
    $build -ne $buildRoot) {
    throw "BuildDirectory must stay under $buildRoot"
}
New-Item -ItemType Directory -Force -Path $build | Out-Null

$env:KVMCTL_PEER_SOURCE = (Resolve-Path -LiteralPath $peer).Path
$env:KVMCTL_LIBSRTP_SOURCE = (Resolve-Path -LiteralPath $lib).Path
$lock = Join-Path $build 'dependencies.lock'
$sdkconfig = Join-Path $build 'sdkconfig'
Write-Host "ESP-IDF: $idfVersion"
Write-Host "Peer source: $env:KVMCTL_PEER_SOURCE"
Write-Host "libSRTP source: $env:KVMCTL_LIBSRTP_SOURCE"
Write-Host "Build directory: $build"

$arguments = @(
    '-C', $project,
    '-B', $build,
    '--no-ccache',
    '-DIDF_TARGET=esp32s3',
    "-DSDKCONFIG=$sdkconfig",
    "-DKVMCTL_DEPENDENCIES_LOCK=$lock",
    'reconfigure',
    'build'
)
& idf.py @arguments
$result = $LASTEXITCODE
if ($result -eq 0) {
    Copy-Item -LiteralPath (Join-Path $project '..\LICENSE') -Destination $build -Force
    Copy-Item -LiteralPath (Join-Path $project '..\THIRD_PARTY_NOTICES.md') -Destination $build -Force
    $noticeDir = Join-Path $build 'notices'
    New-Item -ItemType Directory -Force -Path $noticeDir | Out-Null
    Copy-Item -LiteralPath (Join-Path $peer 'LICENSE') -Destination (Join-Path $noticeDir 'esp_peer-LICENSE') -Force
    Copy-Item -LiteralPath (Join-Path $peer 'LICENSES') -Destination $noticeDir -Recurse -Force
    foreach ($componentRoot in @(
        (Join-Path $project 'managed_components'),
        (Join-Path $build 'managed_components')
    )) {
        if (Test-Path -LiteralPath $componentRoot) {
            foreach ($license in Get-ChildItem -LiteralPath $componentRoot -Recurse -File -Filter 'LICENSE*') {
                $relative = $license.FullName.Substring($componentRoot.Length + 1)
                $destination = Join-Path $noticeDir $relative
                New-Item -ItemType Directory -Force -Path (Split-Path $destination) | Out-Null
                Copy-Item -LiteralPath $license.FullName -Destination $destination -Force
            }
        }
    }
}
exit $result
