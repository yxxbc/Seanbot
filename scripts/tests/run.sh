#!/usr/bin/env bash
# 运行 scripts/tests 下所有 *_test.sh。
set -uo pipefail
dir="$(cd "$(dirname "$0")" && pwd)"
status=0
for t in "$dir"/*_test.sh; do
  bash "$t" || status=1
done
exit $status
