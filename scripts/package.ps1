$ErrorActionPreference = 'Stop'

$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$metadata = & cargo metadata --manifest-path (Join-Path $root 'Cargo.toml') --no-deps --format-version 1 |
    ConvertFrom-Json
if ($LASTEXITCODE) { throw 'Could not read Cargo package metadata' }

$package = $metadata.packages |
    Where-Object { $_.name -eq 'kvmctl' } |
    Select-Object -First 1
if (-not $package) { throw 'Could not find kvmctl package metadata' }

Push-Location $root
try {
    & cargo build --release --locked -p kvmctl --bins
    if ($LASTEXITCODE) { throw 'Release build failed' }

    $stage = Join-Path $root "dist\kvmctl-$($package.version)-windows-x64"
    New-Item -ItemType Directory -Force -Path $stage | Out-Null
    Copy-Item @(
        (Join-Path $metadata.target_directory 'release\kvmctl.exe'),
        (Join-Path $metadata.target_directory 'release\kvmctl-tray.exe'),
        (Join-Path $root 'README.md'),
        (Join-Path $root 'LICENSE'),
        (Join-Path $root 'THIRD_PARTY_NOTICES.md')
    ) -Destination $stage -Force
    Write-Host "Packaged kvmctl to $stage"
}
finally {
    Pop-Location
}
