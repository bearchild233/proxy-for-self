param(
    [string]$Distribution = '',
    [ValidateSet('check', 'test', 'clippy', 'build', 'release')]
    [string]$Task = 'check',
    [string]$Package = 'codex-proxy-rs',
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$CargoArgs = @()
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
# 为 Windows 留出余量；低内存时保持单任务，避免并发编译触发大量换页。
$freeMiB = [math]::Floor((Get-CimInstance Win32_OperatingSystem).FreePhysicalMemory / 1024)
$memoryMiB = 2048
$jobs = 1
if ($freeMiB -ge 6144) { $memoryMiB = 4096; $jobs = 3 }
elseif ($freeMiB -ge 4608) { $memoryMiB = 3072; $jobs = 2 }
$wslArgs = @('-u', 'root')
if ($Distribution) { $wslArgs += @('-d', $Distribution) }
$linuxRepo = & wsl.exe @wslArgs --exec wslpath -a -u $repo.Replace('\', '/')
if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve the repository in WSL.' }
$linuxRepo = $linuxRepo.Trim()
Write-Host "Local Rust: CPU limit 4 cores, memory ${memoryMiB} MiB, $jobs Cargo job(s), incremental dev build."
& wsl.exe @wslArgs --exec bash "$linuxRepo/scripts/build-local.sh" $linuxRepo $memoryMiB $jobs $Task $Package @CargoArgs
if ($LASTEXITCODE -ne 0) { throw "Local Rust build failed (exit $LASTEXITCODE)." }
