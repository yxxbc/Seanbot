#!/bin/sh
# Seanbot 安装脚本（macOS / Linux）
#
#   curl -fsSL https://raw.githubusercontent.com/yxxbc/Seanbot/main/scripts/install.sh | sh
#
# 可选环境变量：
#   SEANBOT_VERSION        安装指定版本（如 0.2.0 或 v0.2.0），默认最新版
#   SEANBOT_INSTALL_DIR    安装目录，默认 ~/.local/bin
#   SEANBOT_DOWNLOAD_BASE  下载地址前缀，默认 https://github.com/yxxbc/Seanbot/releases
#                          （也可以是本地目录，测试时使用）
set -eu

BASE="${SEANBOT_DOWNLOAD_BASE:-https://github.com/yxxbc/Seanbot/releases}"
DIR="${SEANBOT_INSTALL_DIR:-$HOME/.local/bin}"

die() { printf '✗ %s\n' "$*" >&2; exit 1; }

# 系统与架构 → Rust 目标三元组（SEANBOT_OS / SEANBOT_ARCH 仅供测试覆盖）
detect_target() {
  os="${SEANBOT_OS:-$(uname -s)}"
  arch="${SEANBOT_ARCH:-$(uname -m)}"
  case "$os" in
    Darwin) os_part=apple-darwin ;;
    Linux) os_part=unknown-linux-gnu ;;
    MINGW* | MSYS* | CYGWIN* | Windows*) die "Windows 请在 PowerShell 中运行 install.ps1：irm https://raw.githubusercontent.com/yxxbc/Seanbot/main/scripts/install.ps1 | iex" ;;
    *) die "不支持的系统：$os" ;;
  esac
  case "$arch" in
    x86_64 | amd64) arch_part=x86_64 ;;
    arm64 | aarch64) arch_part=aarch64 ;;
    *) die "不支持的架构：$arch" ;;
  esac
  echo "$arch_part-$os_part"
}

# 下载一个文件：http(s) 用 curl / wget，否则按本地路径复制
fetch() { # 地址 输出文件
  case "$1" in
    http://* | https://*)
      if command -v curl >/dev/null 2>&1; then
        curl -fsSL -o "$2" "$1"
      elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$2" "$1"
      else
        die "需要 curl 或 wget"
      fi
      ;;
    *) cp "$1" "$2" ;;
  esac
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    die "需要 sha256sum 或 shasum 来校验下载内容"
  fi
}

target="$(detect_target)"
asset="sean-$target.tar.gz"
if [ -n "${SEANBOT_VERSION:-}" ]; then
  version="${SEANBOT_VERSION#v}"
  url="$BASE/download/v$version"
  label="v$version"
else
  url="$BASE/latest/download"
  label="最新版"
fi

tmp="$(mktemp -d 2>/dev/null || mktemp -d -t seanbot)"
trap 'rm -rf "$tmp"' EXIT INT TERM

printf '▶ 下载 Seanbot %s（%s）\n' "$label" "$target"
fetch "$url/$asset" "$tmp/$asset" 2>/dev/null || die "下载失败：$url/${asset}（版本不存在，或网络不可用）"
fetch "$url/SHA256SUMS" "$tmp/SHA256SUMS" 2>/dev/null || die "下载失败：$url/SHA256SUMS"

expected="$(awk -v f="$asset" '$2 == f { print $1 }' "$tmp/SHA256SUMS")"
[ -n "$expected" ] || die "SHA256SUMS 中没有 $asset"
actual="$(sha256_of "$tmp/$asset")"
[ "$expected" = "$actual" ] || die "下载内容校验失败（SHA256 不一致），已中止安装"

tar -xzf "$tmp/$asset" -C "$tmp"
[ -f "$tmp/sean" ] || die "安装包中没有 sean"
mkdir -p "$DIR"
cp "$tmp/sean" "$DIR/.sean.new.$$"
chmod 755 "$DIR/.sean.new.$$"
mv -f "$DIR/.sean.new.$$" "$DIR/sean"

printf '✓ 已安装到 %s/sean\n' "$DIR"
"$DIR/sean" --version 2>/dev/null || true
case ":$PATH:" in
  *":$DIR:"*) ;;
  *) printf '提示：%s 不在 PATH 中，请把下面这行加入 shell 配置文件（如 ~/.zshrc）：\n  export PATH="%s:$PATH"\n' "$DIR" "$DIR" ;;
esac
