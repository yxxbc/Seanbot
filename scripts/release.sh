#!/usr/bin/env bash
# 一键发版：检查 → 测试 → 改版本号 → 更新 CHANGELOG 与 README 徽章 → 提交并打标签 → 推送（推送前确认）。
# 推送标签后，GitHub Actions 的 release 工作流负责编译各平台产物并发布 Release。
#
# 用法：scripts/release.sh <版本号> [选项]
#   --yes         推送前不再询问
#   --no-push     只在本地提交与打标签，不推送
#   --dry-run     只检查并说明将要做的事，不做任何改动
#   --skip-tests  跳过 scripts/test.sh
set -euo pipefail

die() { printf '✗ %s\n' "$*" >&2; exit 1; }
info() { printf '▶ %s\n' "$*"; }

usage() { sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'; }

version="" yes=0 push=1 dry=0 tests=1
for arg in "$@"; do
  case "$arg" in
    --yes) yes=1 ;;
    --no-push) push=0 ;;
    --dry-run) dry=1 ;;
    --skip-tests) tests=0 ;;
    -h | --help) usage; exit 0 ;;
    -*) die "未知选项：$arg" ;;
    *) [ -z "$version" ] || die "只能指定一个版本号"; version="${arg#v}" ;;
  esac
done
[ -n "$version" ] || { usage; exit 1; }
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "版本号格式应为 X.Y.Z：$version"

cd "$(git rev-parse --show-toplevel)"
tag="v$version"
current="$(bash scripts/cargo-version.sh)"

# 逐段比较 X.Y.Z
version_gt() {
  local IFS=.
  local -a a=($1) b=($2)
  for i in 0 1 2; do
    ((10#${a[i]} > 10#${b[i]})) && return 0
    ((10#${a[i]} < 10#${b[i]})) && return 1
  done
  return 1
}
version_gt "$version" "$current" || die "新版本号必须大于当前版本 $current"

branch="$(git rev-parse --abbrev-ref HEAD)"
[ "$branch" = main ] || die "请在 main 分支上发布（当前：${branch}）"
git diff --quiet && git diff --cached --quiet || die "工作区有未提交的改动，请先提交或暂存"
git rev-parse -q --verify "refs/tags/$tag" >/dev/null && die "标签 $tag 已存在"

# 未发布一节必须有内容
unreleased="$(awk '/^## \[未发布\]/ { f = 1; next } f && /^## \[/ { exit } f && /^[[:space:]]*- / { print }' CHANGELOG.md)"
[ -n "$unreleased" ] || die "CHANGELOG.md 的「未发布」一节为空，请先记录本次变更"

if [ "$dry" = 1 ]; then
  info "将发布 ${tag}（当前 ${current}）"
  echo "  1. 运行 scripts/test.sh$([ "$tests" = 0 ] && echo "（已跳过）")"
  echo "  2. 更新 Cargo.toml / Cargo.lock 版本号为 $version"
  echo "  3. CHANGELOG「未发布」→「[$version] - $(date +%Y-%m-%d)」，README 徽章 → $version"
  echo "  4. 提交 chore(release): 发布 $tag 并打标签 $tag"
  echo "  5. $([ "$push" = 1 ] && echo "推送 main 与 $tag 到 origin" || echo "不推送")"
  exit 0
fi

if [ "$tests" = 1 ]; then
  info "运行检查"
  bash scripts/test.sh
fi

info "更新版本号 $current → $version"
tmp="$(mktemp)"
awk -v ver="$version" '
  /^\[/ { in_pkg = ($0 == "[workspace.package]") }
  in_pkg && /^version[[:space:]]*=/ { print "version = \"" ver "\""; next }
  { print }
' Cargo.toml > "$tmp" && mv "$tmp" Cargo.toml
cargo update --workspace --offline -q

info "更新 CHANGELOG 与 README"
today="$(date +%Y-%m-%d)"
tmp="$(mktemp)"
awk -v ver="$version" -v day="$today" '
  !done && /^## \[未发布\]/ { print; print ""; print "## [" ver "] - " day; done = 1; next }
  { print }
' CHANGELOG.md > "$tmp" && mv "$tmp" CHANGELOG.md
if [ -f README.md ]; then
  tmp="$(mktemp)"
  sed -E "s/badge\/version-[0-9]+\.[0-9]+\.[0-9]+-/badge\/version-$version-/" README.md > "$tmp" && mv "$tmp" README.md
fi

git add Cargo.toml Cargo.lock CHANGELOG.md
[ -f README.md ] && git add README.md
git commit -qm "chore(release): 发布 $tag"
git tag -a "$tag" -m "Seanbot $tag"
info "已提交并打标签 $tag"

manual="git push origin main $tag"
if [ "$push" = 0 ]; then
  echo "未推送。确认无误后运行：$manual"
  exit 0
fi
if [ "$yes" = 0 ]; then
  printf '推送 main 与 %s 到 origin？推送后将自动构建并发布 Release [y/N] ' "$tag"
  read -r answer || answer=""
  case "$answer" in
    y | Y | yes) ;;
    *) echo "未推送。确认无误后运行：$manual"; exit 0 ;;
  esac
fi
git push origin main "$tag"
info "已推送。Release 工作流完成后可在 GitHub 查看：$(git remote get-url origin | sed 's/\.git$//')/releases/tag/$tag"
