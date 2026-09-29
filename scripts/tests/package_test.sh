#!/usr/bin/env bash
# package.sh 的测试：用伪造的编译产物打包。
set -uo pipefail
. "$(dirname "$0")/lib.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "package.sh"

mkdir -p "$TMP/target/x86_64-unknown-linux-gnu/release" "$TMP/target/x86_64-pc-windows-msvc/release"
printf 'unix-bin' > "$TMP/target/x86_64-unknown-linux-gnu/release/sean"
printf 'win-bin' > "$TMP/target/x86_64-pc-windows-msvc/release/sean.exe"

out="$(CARGO_TARGET_DIR="$TMP/target" bash "$ROOT/scripts/package.sh" x86_64-unknown-linux-gnu "$TMP/dist" 2>&1)"; st=$?
assert_status "打包 Unix 产物" 0 "$st"
assert_eq "tar.gz 只含 sean" "sean" "$(tar -tzf "$TMP/dist/sean-x86_64-unknown-linux-gnu.tar.gz")"
assert_contains "输出产物路径" "$out" "sean-x86_64-unknown-linux-gnu.tar.gz"

if command -v zip >/dev/null 2>&1 || command -v 7z >/dev/null 2>&1; then
  out="$(CARGO_TARGET_DIR="$TMP/target" bash "$ROOT/scripts/package.sh" x86_64-pc-windows-msvc "$TMP/dist" 2>&1)"; st=$?
  assert_status "打包 Windows 产物" 0 "$st"
  assert_contains "zip 含 sean.exe" "$(unzip -l "$TMP/dist/sean-x86_64-pc-windows-msvc.zip")" "sean.exe"
fi

out="$(CARGO_TARGET_DIR="$TMP/target" bash "$ROOT/scripts/package.sh" aarch64-apple-darwin "$TMP/dist" 2>&1)"; st=$?
assert_eq "缺少编译产物时报错" "1" "$st"
assert_contains "说明缺少产物" "$out" "找不到"

finish
