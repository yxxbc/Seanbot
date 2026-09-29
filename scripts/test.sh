#!/usr/bin/env bash
# 本地与 CI 共用的检查：格式、clippy、测试，以及发布脚本的测试（Windows 上跳过）。
#
# 用法：scripts/test.sh [--fix]
#   --fix  先自动修复格式与 clippy 可修复的问题，再检查
set -euo pipefail
cd "$(dirname "$0")/.."

fix=0
for arg in "$@"; do
  case "$arg" in
    --fix) fix=1 ;;
    -h | --help) sed -n '2,5p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "未知参数：${arg}" >&2; exit 2 ;;
  esac
done

step() {
  local name="$1"
  shift
  printf '\n▶ %s\n' "$name"
  if "$@"; then
    printf '✓ %s\n' "$name"
  else
    printf '✗ %s 失败\n' "$name" >&2
    exit 1
  fi
}

if [ "$fix" = 1 ]; then
  step "自动格式化" cargo fmt --all
  step "clippy 自动修复" cargo clippy --all-targets --fix --allow-dirty --allow-staged -q
fi

step "格式检查" cargo fmt --all --check
step "clippy" cargo clippy --all-targets -- -D warnings
step "Rust 测试" cargo test
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) echo "（Windows：跳过 shell 脚本测试）" ;;
  *) step "脚本测试" bash scripts/tests/run.sh ;;
esac
printf '\n全部检查通过\n'
