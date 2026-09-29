#!/usr/bin/env bash
# 输出 CHANGELOG 中某个版本一节的正文（不含标题，去掉首尾空行），用作 Release 说明。
# 用法：scripts/changelog-section.sh <版本号> [CHANGELOG 路径]
set -euo pipefail
version="${1:?用法：changelog-section.sh <版本号> [文件]}"
version="${version#v}"
file="${2:-CHANGELOG.md}"
awk -v ver="$version" '
  index($0, "## [" ver "]") == 1 { found = 1; next }
  found && /^## \[/ { exit }
  found { print }
' "$file" | awk 'NF { started = 1 } started' | awk '
  { lines[NR] = $0 }
  END {
    last = NR
    while (last > 0 && lines[last] ~ /^[[:space:]]*$/) last--
    for (i = 1; i <= last; i++) print lines[i]
  }
'
