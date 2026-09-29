# Seanbot 安装脚本（Windows）
#
#   irm https://raw.githubusercontent.com/yxxbc/Seanbot/main/scripts/install.ps1 | iex
#
# 可选环境变量：
#   SEANBOT_VERSION          安装指定版本（如 0.2.0 或 v0.2.0），默认最新版
#   SEANBOT_INSTALL_DIR      安装目录，默认 %LOCALAPPDATA%\Seanbot\bin
#   SEANBOT_DOWNLOAD_BASE    下载地址前缀，默认 https://github.com/yxxbc/Seanbot/releases（也可以是本地目录）
#   SEANBOT_NO_MODIFY_PATH   设为 1 时不修改用户 PATH
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$Base = if ($env:SEANBOT_DOWNLOAD_BASE) { $env:SEANBOT_DOWNLOAD_BASE.TrimEnd('/', '\') } else { 'https://github.com/yxxbc/Seanbot/releases' }
$Dir = if ($env:SEANBOT_INSTALL_DIR) { $env:SEANBOT_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Seanbot\bin' }

$Arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
if ($Arch -ne 'AMD64') { throw "暂不支持的架构：$Arch（目前只提供 x86_64 版本）" }
$Target = 'x86_64-pc-windows-msvc'
$Asset = "sean-$Target.zip"

if ($env:SEANBOT_VERSION) {
    $Version = $env:SEANBOT_VERSION.TrimStart('v')
    $Url = "$Base/download/v$Version"
    $Label = "v$Version"
} else {
    $Url = "$Base/latest/download"
    $Label = '最新版'
}

function Get-RemoteFile([string]$From, [string]$To) {
    if ($From -match '^https?://') {
        Invoke-WebRequest -UseBasicParsing -Uri $From -OutFile $To
    } else {
        Copy-Item -LiteralPath $From -Destination $To
    }
}

$Tmp = Join-Path ([IO.Path]::GetTempPath()) ('seanbot-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $Tmp | Out-Null
try {
    Write-Host "▶ 下载 Seanbot $Label（$Target）"
    $Zip = Join-Path $Tmp $Asset
    $Sums = Join-Path $Tmp 'SHA256SUMS'
    try {
        Get-RemoteFile "$Url/$Asset" $Zip
        Get-RemoteFile "$Url/SHA256SUMS" $Sums
    } catch {
        throw "下载失败：$Url/$Asset（版本不存在，或网络不可用）"
    }

    $Expected = Get-Content $Sums | ForEach-Object {
        $parts = $_ -split '\s+'
        if ($parts.Count -ge 2 -and $parts[1] -eq $Asset) { $parts[0] }
    } | Select-Object -First 1
    if (-not $Expected) { throw "SHA256SUMS 中没有 $Asset" }
    $Actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $Zip).Hash.ToLower()
    if ($Actual -ne $Expected.ToLower()) { throw '下载内容校验失败（SHA256 不一致），已中止安装' }

    Expand-Archive -LiteralPath $Zip -DestinationPath $Tmp -Force
    $Exe = Join-Path $Tmp 'sean.exe'
    if (-not (Test-Path $Exe)) { throw '安装包中没有 sean.exe' }
    New-Item -ItemType Directory -Path $Dir -Force | Out-Null
    Copy-Item -LiteralPath $Exe -Destination (Join-Path $Dir 'sean.exe') -Force
    Write-Host "✓ 已安装到 $(Join-Path $Dir 'sean.exe')"

    if ($env:SEANBOT_NO_MODIFY_PATH -ne '1') {
        $UserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $Entries = if ($UserPath) { $UserPath -split ';' } else { @() }
        if ($Entries -notcontains $Dir) {
            $NewPath = (@($Entries | Where-Object { $_ }) + $Dir) -join ';'
            [Environment]::SetEnvironmentVariable('Path', $NewPath, 'User')
            Write-Host "已把 $Dir 加入用户 PATH，重新打开终端后生效"
        }
    }
} finally {
    Remove-Item -Recurse -Force -LiteralPath $Tmp -ErrorAction SilentlyContinue
}
