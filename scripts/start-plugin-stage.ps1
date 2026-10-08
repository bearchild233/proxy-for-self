param([string]$Distribution = 'ApiHubBuild-20260928', [switch]$RestartGateway)
$ErrorActionPreference = 'Stop'
$stageRepo = Split-Path $PSScriptRoot -Parent
$stageScript = Join-Path $PSScriptRoot 'plugin-stage-wsl.py'
$stageWslScript = & wsl -d $Distribution -- wslpath -a $stageScript.Replace('\', '/')
if ($LASTEXITCODE -ne 0) { throw 'Unable to resolve staging script in WSL' }
$stageWslScript = $stageWslScript.Trim()
$stageGatewayArgs = @($stageWslScript)
if ($RestartGateway) { $stageGatewayArgs += '--restart' }
& wsl -d $Distribution -u root -- python3 @stageGatewayArgs
if ($LASTEXITCODE -ne 0) { throw 'Local staging gateway failed to start' }

$stageListener = Get-NetTCPConnection -LocalPort 18335 -State Listen -ErrorAction SilentlyContinue
if ($stageListener) {
    $stageHealth = Invoke-RestMethod 'http://127.0.0.1:18335/healthz' -TimeoutSec 5
    if ($stageHealth.service -ne 'plugin-platform') { throw 'Port 18335 belongs to another application' }
    Write-Output 'Local plugin stage is already running: http://127.0.0.1:18335/'
    exit 0
}
$stagePython = (Get-Command python -ErrorAction Stop).Source
$stageState = Join-Path $stageRepo '.build/plugin-stage'
$stageProcess = Start-Process -FilePath $stagePython -ArgumentList @('-X', 'utf8', 'services/plugin_host/server.py', 'serve', '--root', '.build/plugin-platform', '--local-access', '.build/plugin-stage/access.json', '--local-excel-wsl', $Distribution) `
    -WorkingDirectory $stageRepo -WindowStyle Hidden `
    -RedirectStandardOutput (Join-Path $stageState 'shell.log') `
    -RedirectStandardError (Join-Path $stageState 'shell-error.log') -PassThru
$stageProcess.Id | Set-Content (Join-Path $stageState 'shell.pid')
for ($stageAttempt = 0; $stageAttempt -lt 20; $stageAttempt++) {
    try {
        $stageHealth = Invoke-RestMethod 'http://127.0.0.1:18335/healthz' -TimeoutSec 2
        if ($stageHealth.ok) {
            Write-Output 'Local plugin stage: http://127.0.0.1:18335/'
            exit 0
        }
    } catch { Start-Sleep -Milliseconds 250 }
}
throw 'Local staging shell did not become ready; inspect .build/plugin-stage/shell-error.log'
