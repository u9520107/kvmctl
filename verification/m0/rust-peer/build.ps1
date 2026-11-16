param([string]$WebrtcSource = (Join-Path $env:TEMP 'webrtc-rs-0.21.1'))
$ErrorActionPreference = 'Stop'
$source = (Resolve-Path -LiteralPath $WebrtcSource).Path.Replace('\', '/')
$revision = git -c "safe.directory=$source" -C $WebrtcSource rev-parse HEAD
if ($LASTEXITCODE -ne 0 -or $revision -ne 'd658b5a803dd8dc389547450e245877310c9cbed') {
    throw 'Expected recorded webrtc-rs v0.21.1 checkout.'
}
git -c "safe.directory=$source" -C $WebrtcSource diff --quiet HEAD -- src Cargo.toml
if ($LASTEXITCODE -ne 0) { throw 'WebRTC library differs from recorded revision.' }
$rtc = Join-Path $WebrtcSource 'rtc'
$rtcSource = $rtc.Replace('\', '/')
$rtcRevision = git -c "safe.directory=$rtcSource" -C $rtc rev-parse HEAD
if ($LASTEXITCODE -ne 0 -or $rtcRevision -ne '2a732df55ca4bcf26b80914fa0f570cfaf74cfcb') {
    throw 'RTC submodule differs from recorded revision.'
}
git -c "safe.directory=$rtcSource" -C $rtc diff --quiet HEAD -- src Cargo.toml 'rtc-*'
if ($LASTEXITCODE -ne 0) { throw 'RTC library differs from recorded revision.' }
if ($source.Contains("'")) { throw 'Source path must not contain a single quote.' }
$config = Join-Path $PSScriptRoot 'local-cargo.toml'
[IO.File]::WriteAllText($config, "[patch.crates-io]`nwebrtc = { path = '$source' }`n")
Set-Location $PSScriptRoot
cargo --config $config build --offline
exit $LASTEXITCODE
