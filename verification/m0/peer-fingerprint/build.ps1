param(
    [string]$WebrtcSource = (Join-Path $env:TEMP 'esp-peer-v1.3.0'),
    [string]$ProjectDirectory = $PSScriptRoot,
    [switch]$Harden
)
$ErrorActionPreference = 'Stop'
$project = (Resolve-Path -LiteralPath $ProjectDirectory).Path
if (-not (Test-Path -LiteralPath (Join-Path $project 'CMakeLists.txt'))) {
    throw 'ProjectDirectory must contain an ESP-IDF CMakeLists.txt.'
}
$localPeer = Join-Path $project 'third_party/esp_peer'
$useLocalPeer = Test-Path -LiteralPath (Join-Path $localPeer 'CMakeLists.txt')
if ($useLocalPeer) {
    $env:KVMCTL_PEER_SOURCE = $localPeer
    Write-Output "Using JetKVM Mini source-built esp_peer: $localPeer"
} else {
    $trustedSource = (Resolve-Path -LiteralPath $WebrtcSource).Path.Replace('\', '/')
    $revision = git -c "safe.directory=$trustedSource" -C $WebrtcSource rev-parse HEAD
    if ($LASTEXITCODE -ne 0 -or $revision -ne '3971094e2e7c2ab7cb4de59e4eae710e1902d2b4') {
        throw 'Expected the recorded esp-webrtc-solution v1.3.0 revision.'
    }
    git -c "safe.directory=$trustedSource" -C $WebrtcSource diff --quiet HEAD -- components/esp_peer/src components/esp_peer/include components/esp_peer/libs components/esp_peer/CMakeLists.txt components/esp_peer/idf_component.yml
    if ($LASTEXITCODE -ne 0) { throw 'Peer implementation differs from the recorded revision.' }
    $env:KVMCTL_PEER_SOURCE = Join-Path $WebrtcSource 'components/esp_peer'
}
. C:/Espressif/tools/Microsoft.v6.1.1.PowerShell_profile.ps1
$env:IDF_CCACHE_ENABLE = '0'
$env:CCACHE_DISABLE = '1'
Set-Location $PSScriptRoot
if ($Harden -and $useLocalPeer) {
    $source = Get-Content -LiteralPath (Join-Path $localPeer 'src/esp_peer.c') -Raw
    if (-not $source.Contains('dtls_fingerprint_expect')) { throw 'Local Mini peer source lacks SDP fingerprint enforcement.' }
} elseif ($Harden) {
    $patch = Join-Path $PSScriptRoot 'esp-peer-fingerprint.patch'
    $sha = [Security.Cryptography.SHA256]::Create()
    try { $patchHash = [BitConverter]::ToString($sha.ComputeHash([IO.File]::ReadAllBytes($patch))).Replace('-', '').ToLowerInvariant() }
    finally { $sha.Dispose() }
    $vendor = Join-Path $PSScriptRoot ".vendor/$patchHash"
    if (-not (Test-Path -LiteralPath (Join-Path $vendor 'ready'))) {
        [void][IO.Directory]::CreateDirectory($vendor)
        $archive = Join-Path $vendor 'upstream.zip'
        git -c "safe.directory=$trustedSource" -C $WebrtcSource archive --format=zip --output=$archive HEAD components/esp_peer
        if ($LASTEXITCODE -ne 0) { throw 'Could not export pinned peer source.' }
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $extract = Join-Path $vendor 'extract'
        [IO.Compression.ZipFile]::ExtractToDirectory($archive, $extract)
        Move-Item -LiteralPath (Join-Path $extract 'components') -Destination $vendor
        Remove-Item -LiteralPath $extract -Recurse
        # git apply filters paths when invoked below the worktree root.
        $workspace = (Resolve-Path (Join-Path $PSScriptRoot '../../..')).Path
        git -C $workspace apply --unsafe-paths --directory=$vendor $patch
        if ($LASTEXITCODE -ne 0) { throw 'Hardening patch did not apply to pinned source.' }
        git -C $workspace apply --reverse --check --unsafe-paths --directory=$vendor $patch
        if ($LASTEXITCODE -ne 0) { throw 'Exported source does not contain the complete patch.' }
        [IO.File]::WriteAllText((Join-Path $vendor 'ready'), $patchHash)
    }
    if (-not ([IO.File]::ReadAllText((Join-Path $vendor 'components/esp_peer/src/esp_peer.c')).Contains('esp_peer_remote_fingerprint_matches'))) {
        throw 'Cached peer source lacks fingerprint enforcement; remove its ready marker and rebuild.'
    }
    $env:KVMCTL_PEER_SOURCE = Join-Path $vendor 'components/esp_peer'
    Write-Output "M0 patched peer: $patchHash"
}
$lock = Join-Path $project 'dependencies.lock'
$workspace = (git -C $project rev-parse --show-toplevel).Trim()
$relativeLock = $lock.Substring($workspace.Length + 1).Replace('\', '/')
git -C $workspace ls-files --error-unmatch -- $relativeLock *> $null
$trackedLock = $LASTEXITCODE -eq 0
$lockBackup = if ($trackedLock) { [IO.File]::ReadAllBytes($lock) } else { $null }
$buildResult = 1
try {
    if (Test-Path -LiteralPath $lock) { Remove-Item -LiteralPath $lock }
    Set-Location $project
    idf.py --no-ccache reconfigure build
    $buildResult = $LASTEXITCODE
} finally {
    if ($trackedLock) { [IO.File]::WriteAllBytes($lock, $lockBackup) }
}
exit $buildResult
