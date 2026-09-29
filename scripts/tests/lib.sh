# 脚本测试的公共断言。用法：在测试文件中 `. "$(dirname "$0")/lib.sh"`。
# shellcheck shell=bash

PASS=0
FAIL=0
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

pass() { PASS=$((PASS + 1)); printf '  ✓ %s\n' "$1"; }
fail() { FAIL=$((FAIL + 1)); printf '  ✗ %s\n' "$1"; [ -n "${2:-}" ] && printf '%s\n' "$2" | sed 's/^/      /'; }

assert_eq() { # 名称 期望 实际
  if [ "$2" = "$3" ]; then pass "$1"; else fail "$1" "期望：$2"$'\n'"实际：$3"; fi
}

assert_contains() { # 名称 文本 子串
  case "$2" in *"$3"*) pass "$1" ;; *) fail "$1" "缺少：$3"$'\n'"文本：$2" ;; esac
}

assert_not_contains() { # 名称 文本 子串
  case "$2" in *"$3"*) fail "$1" "不应包含：$3" ;; *) pass "$1" ;; esac
}

assert_status() { # 名称 期望退出码 实际退出码
  assert_eq "$1" "$2" "$3"
}

finish() {
  printf '%s：%d 通过，%d 失败\n' "$(basename "$0")" "$PASS" "$FAIL"
  [ "$FAIL" -eq 0 ]
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
  else shasum -a 256 "$1" | awk '{print $1}'; fi
}
