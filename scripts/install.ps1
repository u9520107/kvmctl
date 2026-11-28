param(
    [string]$InstallDirectory = (Join-Path $env:USERPROFILE 'tools\kvmctl'),
    [switch]$NoStart,
    [switch]$NoStartup
)

$ErrorActionPreference = 'Stop'
$provider = $null
$InstallDirectory = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath(
    $InstallDirectory, [ref]$provider, [ref]$null)
if ($provider.Name -ne 'FileSystem') { throw 'InstallDirectory must be a filesystem path.' }
$dist = Join-Path $PSScriptRoot '..\dist'
$files = 'kvmctl.exe', 'kvmctl-tray.exe', 'README.md', 'LICENSE', 'THIRD_PARTY_NOTICES.md'
$build = Get-ChildItem -Path $dist -Directory -Filter 'kvmctl-*-windows-x64' |
    ForEach-Object {
        $directory = $_.FullName
        $missing = $files | Where-Object { -not (Test-Path (Join-Path $directory $_)) }
        if (-not $missing) {
            [pscustomobject]@{
                Path = $directory
                BuildTime = (Get-Item (Join-Path $directory 'kvmctl-tray.exe')).LastWriteTimeUtc
            }
        }
    } |
    Sort-Object BuildTime -Descending |
    Select-Object -First 1
if (-not $build) { throw "No portable build found in $dist" }

$cliPath = Join-Path $InstallDirectory 'kvmctl.exe'
$trayPath = Join-Path $InstallDirectory 'kvmctl-tray.exe'
$running = @(Get-Process -Name kvmctl,kvmctl-tray -ErrorAction SilentlyContinue |
    Where-Object {
        try { $_.Path -ieq $cliPath -or $_.Path -ieq $trayPath } catch { $false }
    })

New-Item -ItemType Directory -Force -Path $InstallDirectory | Out-Null
if ($running) {
    $running | Stop-Process -ErrorAction Stop
    foreach ($process in $running) {
        Wait-Process -Id $process.Id -Timeout 10 -ErrorAction SilentlyContinue
        if (Get-Process -Id $process.Id -ErrorAction SilentlyContinue) {
            throw "Could not stop $($_.ProcessName)"
        }
    }
}
Copy-Item ($files | ForEach-Object { Join-Path $build.Path $_ }) -Destination $InstallDirectory -Force -ErrorAction Stop

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$pathItems = @($userPath -split ';' | Where-Object { $_ })
if (-not ($pathItems | Where-Object { $_.TrimEnd('\') -ieq $InstallDirectory.TrimEnd('\') })) {
    [Environment]::SetEnvironmentVariable('Path', ($pathItems + $InstallDirectory) -join ';', 'User')
}
$env:Path = "$env:Path;$InstallDirectory"

$shortcut = Join-Path ([Environment]::GetFolderPath('Startup')) 'Kvmctl.lnk'
if (-not $NoStartup) {
    $shell = New-Object -ComObject WScript.Shell
    $link = $shell.CreateShortcut($shortcut)
    $link.TargetPath = $trayPath
    $link.WorkingDirectory = $InstallDirectory
    $link.IconLocation = "$trayPath,0"
    $link.Save()
}

if (-not $NoStart) {
    Start-Process -FilePath $trayPath -WorkingDirectory $InstallDirectory -ErrorAction Stop
}

Write-Host "Installed kvmctl to $InstallDirectory"
