param(
    [string]$WebrtcSource = (Join-Path $env:TEMP 'esp-peer-v1.3.0'),
    [switch]$Harden
)
$ErrorActionPreference = 'Stop'
$trustedSource = (Resolve-Path -LiteralPath $WebrtcSource).Path.Replace('\', '/')
$revision = git -c "safe.directory=$trustedSource" -C $WebrtcSource rev-parse HEAD
if ($LASTEXITCODE -ne 0 -or $revision -ne '3971094e2e7c2ab7cb4de59e4eae710e1902d2b4') {
    throw 'Expected the recorded esp-webrtc-solution v1.3.0 revision.'
}
git -c "safe.directory=$trustedSource" -C $WebrtcSource diff --quiet HEAD -- components/esp_peer/src components/esp_peer/include components/esp_peer/libs components/esp_peer/CMakeLists.txt components/esp_peer/idf_component.yml
if ($LASTEXITCODE -ne 0) { throw 'Peer implementation differs from the recorded revision.' }
$env:KVMCTL_WEBRTC_SOURCE = $WebrtcSource
$env:KVMCTL_PEER_SOURCE = Join-Path $WebrtcSource 'components/esp_peer'
. C:/Espressif/tools/Microsoft.v6.1.1.PowerShell_profile.ps1
$env:IDF_CCACHE_ENABLE = '0'
$env:CCACHE_DISABLE = '1'
Set-Location $PSScriptRoot
if ($Harden) {
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
        Expand-Archive -LiteralPath $archive -DestinationPath $vendor -Force
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
idf.py --no-ccache build
exit $LASTEXITCODE
