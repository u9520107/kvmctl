param(
    [switch]$Online
)

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location $root

function Invoke-Checked {
    param(
        [string]$Name,
        [scriptblock]$Command
    )

    Write-Host "== $Name =="
    & $Command
    if ($LASTEXITCODE -ne 0) {
        throw "$Name failed with exit code $LASTEXITCODE"
    }
}

$python = Get-Command python.exe -ErrorAction SilentlyContinue
$cargo = Get-Command cargo.exe -ErrorAction SilentlyContinue
if (-not $python) { throw 'Missing Python prerequisite: install Python 3 and put python.exe on PATH.' }
if (-not $cargo) { throw 'Missing Rust prerequisite: install Cargo and put cargo.exe on PATH.' }

$windows = $env:OS -eq 'Windows_NT'
$compiler = $null
if ($windows) {
    $compiler = Get-Command cl.exe -ErrorAction SilentlyContinue
    $linker = Get-Command link.exe -ErrorAction SilentlyContinue
    if (-not $compiler -or -not $linker) {
        throw 'Missing C compiler prerequisites: run from a Visual Studio Developer PowerShell with cl.exe and link.exe on PATH. The seven-check run cannot silently skip compiler-backed tests.'
    }
    Write-Host "C compiler: $($compiler.Source)"
    Write-Host "C linker: $($linker.Source)"
} else {
    $compiler = Get-Command clang -ErrorAction SilentlyContinue
    if (-not $compiler) { $compiler = Get-Command cc -ErrorAction SilentlyContinue }
    if (-not $compiler) { throw 'Missing C compiler prerequisite: install clang or cc before running the host regression command.' }
    Write-Host "C compiler: $($compiler.Source)"
}

$fixture = Join-Path $root 'firmware/build/host-regression/live-input-fixture.bin'
New-Item -ItemType Directory -Force -Path (Split-Path $fixture) | Out-Null
$liveArgs = @('firmware/test_live_input.py', '--fixture', $fixture, '--cc', $compiler.Source)
Invoke-Checked 'live-input fixture and firmware arm/rearm check' { & $python.Source @liveArgs }
$env:KVMCTL_LIVE_FIXTURE = (Resolve-Path $fixture).Path
Write-Host "KVMCTL_LIVE_FIXTURE=$env:KVMCTL_LIVE_FIXTURE"

if ($windows) {
    $signalingArgs = @('firmware/test_signaling_guards.py', '--msvc', $compiler.Source)
} else {
    $signalingArgs = @('firmware/test_signaling_guards.py', '--clang', $compiler.Source)
}

$checks = @(
    [pscustomobject]@{ Name = 'lease cleanup guards'; Arguments = @('firmware/test_lease_cleanup_guards.py') },
    [pscustomobject]@{ Name = 'BOOT recovery guards'; Arguments = @('firmware/test_boot_recovery_guards.py') },
    [pscustomobject]@{ Name = 'production HID guards'; Arguments = @('firmware/test_production_hid_guards.py') },
    [pscustomobject]@{ Name = 'signaling guards'; Arguments = $signalingArgs },
    [pscustomobject]@{ Name = 'USB administration guards'; Arguments = @('firmware/test_usb_admin.py') },
    [pscustomobject]@{ Name = 'Wi-Fi setup guards'; Arguments = @('firmware/test_wifi_setup_guards.py') }
)
foreach ($check in $checks) {
    Invoke-Checked $check.Name { & $python.Source @($check.Arguments) }
}

$cargoArgs = @('test', '--workspace', '--locked')
if (-not $Online) { $cargoArgs += '--offline' }
Invoke-Checked 'ignored live-input fixture handoff test' {
    & $cargo.Source @($cargoArgs + @('firmware_rule_rearm_and_capture_handoff', '--', '--ignored'))
}
Invoke-Checked 'Rust workspace tests' { & $cargo.Source @cargoArgs }

Write-Host 'Host regression passed. Hardware checks remain opt-in; no firmware build or flash was run.'
