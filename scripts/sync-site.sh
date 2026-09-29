#!/usr/bin/env bash
# 把工作区版本同步进站点：写 site/assets/version.json（页面用它显示「最新版本」，
# 用户不必点进 GitHub 才知道当前版本）。
#
# 调用点：
#   - scripts/release.sh：发布提交前调用，让仓库里的副本也保持同步
#   - .github/workflows/pages.yml：部署前跑一次，保证线上永远跟仓库一致
# 仓库里没有 site/ 时安静跳过（release 的测试仓库就是这样）。
set -euo pipefail
cd "$(dirname "$0")/.."

[ -d site ] || exit 0

version="$(bash scripts/cargo-version.sh)"
tag="v$version"

# 发布日期优先取 CHANGELOG 里该版本的日期，取不到就用今天
released="$(awk -v head="[$version]" '
  $1 == "##" && $2 == head {
    for (i = 3; i <= NF; i++) {
      if ($i ~ /^[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]$/) { print $i; exit }
    }
  }
' CHANGELOG.md)"
[ -n "$released" ] || released="$(date +%Y-%m-%d)"

mkdir -p site/assets
tmp="site/assets/version.json.tmp"
printf '{\n  "version": "%s",\n  "tag": "%s",\n  "released": "%s"\n}\n' \
  "$version" "$tag" "$released" >"$tmp"
mv "$tmp" site/assets/version.json
echo "站点版本已同步：${tag}（${released}）"
