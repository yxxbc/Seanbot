#!/usr/bin/env bash
# 脚本可移植性守护：macOS 自带的 bash 3.2 在非 UTF-8 locale 下，
# `$var` 后面紧跟中文标点会被当成变量名的一部分，直接报 unbound variable。
# 规则：变量后面紧跟非 ASCII 字符时必须写成 ${var}。
set -uo pipefail
. "$(dirname "$0")/lib.sh"

echo "脚本可移植性"

hits=""
while IFS= read -r line; do
  [ -n "$line" ] && hits="${hits}${line}"$'\n'
done < <(LC_ALL=C grep -rnE '[$][A-Za-z_][A-Za-z0-9_]*[^ -~]' "$ROOT/scripts" --include='*.sh')

if [ -z "$hits" ]; then
  pass '脚本里没有 $var 紧跟非 ASCII 字符的写法'
else
  fail '存在 $var 紧跟非 ASCII 的写法（bash 3.2 会把它当成变量名）' "$hits"
fi

finish
