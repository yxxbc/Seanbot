#!/usr/bin/env bash
# 把 cargo build --release --target <target> 的产物打包为发布文件。
# 用法：scripts/package.sh <target> <输出目录>
#   Unix：  <输出目录>/sean-<target>.tar.gz（内含 sean）
#   Windows：<输出目录>/sean-<target>.zip（内含 sean.exe）
set -euo pipefail

target="${1:?用法：package.sh <target> <输出目录>}"
out="${2:?用法：package.sh <target> <输出目录>}"
build="${CARGO_TARGET_DIR:-target}/${target}/release"
mkdir -p "$out"
out="$(cd "$out" && pwd)"

case "$target" in
  *windows*)
    bin="$build/sean.exe"
    [ -f "$bin" ] || { echo "✗ 找不到编译产物：${bin}" >&2; exit 1; }
    file="$out/sean-${target}.zip"
    rm -f "$file"
    if command -v zip >/dev/null 2>&1; then
      zip -jq "$file" "$bin"
    else
      7z a -tzip -bso0 "$file" "$bin"
    fi
    ;;
  *)
    bin="$build/sean"
    [ -f "$bin" ] || { echo "✗ 找不到编译产物：${bin}" >&2; exit 1; }
    file="$out/sean-${target}.tar.gz"
    tar -czf "$file" -C "$build" sean
    ;;
esac
echo "$file"
