# install.ps1 的测试（仅在 Windows CI 上运行）：用本地伪造的 Release 目录，不访问网络。
$ErrorActionPreference = 'Stop'
$Root = Resolve-Path (Join-Path $PSScriptRoot '..\..')
$Script = Join-Path $Root 'scripts\install.ps1'
$Tmp = Join-Path ([IO.Path]::GetTempPath()) ('seanbot-test-' + [guid]::NewGuid())
$script:Failed = 0

function Check([string]$Name, [bool]$Ok, [string]$Detail = '') {
    if ($Ok) { Write-Host "  ✓ $Name" } else { Write-Host "  ✗ $Name $Detail"; $script:Failed++ }
}

function New-Release([string]$Dir, [string]$Content) {
    New-Item -ItemType Directory -Path $Dir -Force | Out-Null
    $stage = Join-Path $Tmp ('stage-' + [guid]::NewGuid())
    New-Item -ItemType Directory -Path $stage | Out-Null
    Set-Content -LiteralPath (Join-Path $stage 'sean.exe') -Value $Content -NoNewline
    $zip = Join-Path $Dir 'sean-x86_64-pc-windows-msvc.zip'
    Compress-Archive -Path (Join-Path $stage 'sean.exe') -DestinationPath $zip -Force
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash.ToLower()
    Set-Content -LiteralPath (Join-Path $Dir 'SHA256SUMS') -Value "$hash  sean-x86_64-pc-windows-msvc.zip"
}

function Invoke-Install([hashtable]$EnvVars) {
    $names = @('SEANBOT_DOWNLOAD_BASE', 'SEANBOT_INSTALL_DIR', 'SEANBOT_VERSION')
    foreach ($n in $names) { Remove-Item "Env:$n" -ErrorAction SilentlyContinue }
    foreach ($k in $EnvVars.Keys) { Set-Item "Env:$k" $EnvVars[$k] }
    $env:SEANBOT_NO_MODIFY_PATH = '1'
    try { & pwsh -NoProfile -ExecutionPolicy Bypass -File $Script *>&1 | Out-String; $LASTEXITCODE }
    catch { $_ | Out-String; 1 }
}

Write-Host 'install.ps1'
try {
    $base = Join-Path $Tmp 'releases'
    New-Release (Join-Path $base 'latest\download') 'latest'
    New-Release (Join-Path $base 'download\v0.1.0') 'v0.1.0'

    $dest = Join-Path $Tmp 'bin1'
    $r = Invoke-Install @{ SEANBOT_DOWNLOAD_BASE = $base; SEANBOT_INSTALL_DIR = $dest }
    Check '安装最新版' ((Get-Content -Raw (Join-Path $dest 'sean.exe')) -eq 'latest') ($r | Out-String)

    $dest = Join-Path $Tmp 'bin2'
    $r = Invoke-Install @{ SEANBOT_DOWNLOAD_BASE = $base; SEANBOT_INSTALL_DIR = $dest; SEANBOT_VERSION = 'v0.1.0' }
    Check '安装指定版本' ((Get-Content -Raw (Join-Path $dest 'sean.exe')) -eq 'v0.1.0') ($r | Out-String)

    $bad = Join-Path $Tmp 'bad'
    New-Release (Join-Path $bad 'latest\download') 'x'
    Set-Content -LiteralPath (Join-Path $bad 'latest\download\SHA256SUMS') -Value ('0' * 64 + '  sean-x86_64-pc-windows-msvc.zip')
    $dest = Join-Path $Tmp 'bin3'
    $r = Invoke-Install @{ SEANBOT_DOWNLOAD_BASE = $bad; SEANBOT_INSTALL_DIR = $dest }
    Check '校验失败时不安装' (-not (Test-Path (Join-Path $dest 'sean.exe'))) ($r | Out-String)
    Check '提示校验失败' (($r | Out-String) -match '校验失败') ($r | Out-String)
} finally {
    Remove-Item -Recurse -Force -LiteralPath $Tmp -ErrorAction SilentlyContinue
}
if ($script:Failed -gt 0) { Write-Host "install_test.ps1：$($script:Failed) 项失败"; exit 1 }
Write-Host 'install_test.ps1：全部通过'
# 显式 exit 0：pwsh 会把最后一个原生命令的退出码当成进程退出码，
# 而上面刻意跑的失败安装会留下 $LASTEXITCODE=1
exit 0
