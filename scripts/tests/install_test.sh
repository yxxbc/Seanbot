#!/usr/bin/env bash
# install.sh 的测试：用本地伪造的 Release 目录，不访问网络。
set -uo pipefail
. "$(dirname "$0")/lib.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# 在 <base>/<子路径> 下放一个伪造的发布包：sean 是一个打印版本号的脚本
make_release() { # base 子路径 target 版本
  local dir="$1/$2" stage
  stage="$(mktemp -d "$TMP/stage.XXXX")"
  mkdir -p "$dir"
  printf '#!/bin/sh\necho "sean %s"\n' "$4" > "$stage/sean"
  chmod 755 "$stage/sean"
  tar -czf "$dir/sean-$3.tar.gz" -C "$stage" sean
  (cd "$dir" && printf '%s  %s\n' "$(sha256 "sean-$3.tar.gz")" "sean-$3.tar.gz" >> SHA256SUMS)
}

run_install() { # 额外环境变量通过 env 传入
  env SEANBOT_DOWNLOAD_BASE="$BASE" "$@" sh "$ROOT/scripts/install.sh" 2>&1
}

BASE="$TMP/releases"
make_release "$BASE" latest/download x86_64-unknown-linux-gnu 0.2.0
make_release "$BASE" download/v0.1.0 x86_64-unknown-linux-gnu 0.1.0
make_release "$BASE" latest/download aarch64-apple-darwin 0.2.0

echo "install.sh"

dest="$TMP/bin1"
out="$(run_install SEANBOT_OS=Linux SEANBOT_ARCH=x86_64 SEANBOT_INSTALL_DIR="$dest")"; st=$?
assert_status "安装最新版成功" 0 "$st"
assert_contains "输出安装位置" "$out" "已安装到 $dest/sean"
assert_eq "安装的程序可执行" "sean 0.2.0" "$("$dest/sean" --version 2>&1)"
assert_contains "目录不在 PATH 时给出提示" "$out" "不在 PATH 中"

dest="$TMP/bin2"
out="$(run_install SEANBOT_OS=Linux SEANBOT_ARCH=amd64 SEANBOT_VERSION=v0.1.0 SEANBOT_INSTALL_DIR="$dest")"; st=$?
assert_status "安装指定版本成功" 0 "$st"
assert_eq "安装的是指定版本" "sean 0.1.0" "$("$dest/sean" --version 2>&1)"

dest="$TMP/bin3"
out="$(run_install SEANBOT_OS=Darwin SEANBOT_ARCH=arm64 SEANBOT_INSTALL_DIR="$dest")"; st=$?
assert_status "macOS arm64 选择对应产物" 0 "$st"
assert_contains "输出目标平台" "$out" "aarch64-apple-darwin"

dest="$TMP/bin4"
out="$(PATH="$dest:$PATH" run_install SEANBOT_OS=Linux SEANBOT_ARCH=x86_64 SEANBOT_INSTALL_DIR="$dest")"
assert_not_contains "目录已在 PATH 时不提示" "$out" "不在 PATH 中"

# 校验和不一致：改坏 SHA256SUMS
BAD="$TMP/bad"
make_release "$BAD" latest/download x86_64-unknown-linux-gnu 0.2.0
sed -i.bak 's/^[0-9a-f]\{8\}/deadbeef/' "$BAD/latest/download/SHA256SUMS"
dest="$TMP/bin5"
out="$(env SEANBOT_DOWNLOAD_BASE="$BAD" SEANBOT_OS=Linux SEANBOT_ARCH=x86_64 SEANBOT_INSTALL_DIR="$dest" sh "$ROOT/scripts/install.sh" 2>&1)"; st=$?
assert_eq "校验失败时退出码非零" "1" "$st"
assert_contains "提示校验失败" "$out" "校验失败"
assert_eq "校验失败时不安装" "no" "$([ -e "$dest/sean" ] && echo yes || echo no)"

out="$(run_install SEANBOT_OS=MINGW64_NT-10.0 SEANBOT_ARCH=x86_64 SEANBOT_INSTALL_DIR="$TMP/bin6")"; st=$?
assert_eq "不支持的系统报错" "1" "$st"
assert_contains "Windows 提示改用 install.ps1" "$out" "install.ps1"

out="$(run_install SEANBOT_OS=Linux SEANBOT_ARCH=riscv64 SEANBOT_INSTALL_DIR="$TMP/bin7")"; st=$?
assert_eq "不支持的架构报错" "1" "$st"
assert_contains "说明不支持的架构" "$out" "riscv64"

out="$(run_install SEANBOT_OS=Linux SEANBOT_ARCH=x86_64 SEANBOT_VERSION=9.9.9 SEANBOT_INSTALL_DIR="$TMP/bin8")"; st=$?
assert_eq "版本不存在时报错" "1" "$st"
assert_contains "说明下载失败" "$out" "下载失败"

finish
