#!/usr/bin/env bash
# release.sh 与 changelog-section.sh 的测试：在临时 git 仓库里运行，推送到本地裸仓库。
set -uo pipefail
. "$(dirname "$0")/lib.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
TODAY="$(date +%Y-%m-%d)"

# 建一个最小的 cargo workspace 仓库，带 CHANGELOG 与 README 徽章
new_repo() { # 目录 [未发布内容]
  local repo="$1" unreleased="${2-- 新功能 A}"
  mkdir -p "$repo/scripts" "$repo/a/src"
  cp "$ROOT/scripts/release.sh" "$ROOT/scripts/cargo-version.sh" "$ROOT/scripts/changelog-section.sh" "$ROOT/scripts/sync-site.sh" "$repo/scripts/"
  cat > "$repo/Cargo.toml" <<'TOML'
[workspace]
resolver = "3"
members = ["a"]

[workspace.package]
version = "0.1.0"
edition = "2024"
TOML
  printf '[package]\nname = "a"\nversion.workspace = true\nedition.workspace = true\n' > "$repo/a/Cargo.toml"
  : > "$repo/a/src/lib.rs"
  printf '# 更新日志\n\n## [未发布]\n\n### 新增\n\n%s\n\n## [0.1.0] - 2026-01-01\n\n- 初始版本\n' "$unreleased" > "$repo/CHANGELOG.md"
  printf '![Version](https://img.shields.io/badge/version-0.1.0-blue)\n' > "$repo/README.md"
  (
    cd "$repo" &&
      git init -q -b main &&
      git config user.email t@example.com && git config user.name tester &&
      cargo generate-lockfile --offline -q &&
      git add -A && git commit -qm "chore: 初始化"
  )
}

release() { (cd "$1" && shift && bash scripts/release.sh "$@" 2>&1); }

echo "release.sh"

# 1. --dry-run 不改动任何东西
R="$TMP/r1"; new_repo "$R"
out="$(release "$R" 0.2.0 --dry-run --skip-tests)"; st=$?
assert_status "dry-run 成功" 0 "$st"
[ "$st" = 0 ] || printf '    dry-run 输出：%s\n' "$out"
assert_contains "dry-run 说明将要做的事" "$out" "v0.2.0"
assert_eq "dry-run 不改工作区" "" "$(cd "$R" && git status --porcelain)"
assert_eq "dry-run 不打标签" "" "$(cd "$R" && git tag)"

# 2. --no-push：改版本号、Cargo.lock、CHANGELOG、README，提交并打标签
R="$TMP/r2"; new_repo "$R"
out="$(release "$R" v0.2.0 --no-push --skip-tests)"; st=$?
assert_status "发布（不推送）成功" 0 "$st"
assert_eq "Cargo.toml 版本已更新" "0.2.0" "$(cd "$R" && bash scripts/cargo-version.sh)"
assert_contains "Cargo.lock 已同步" "$(cat "$R/Cargo.lock")" $'name = "a"\nversion = "0.2.0"'
assert_contains "CHANGELOG 新增版本标题" "$(cat "$R/CHANGELOG.md")" $'## [未发布]\n\n## [0.2.0] - '"$TODAY"$'\n\n### 新增\n\n- 新功能 A'
assert_contains "README 徽章已更新" "$(cat "$R/README.md")" "version-0.2.0-blue"
assert_eq "提交信息" "chore(release): 发布 v0.2.0" "$(cd "$R" && git log -1 --format=%s)"
assert_eq "标签指向发布提交" "$(cd "$R" && git rev-parse HEAD)" "$(cd "$R" && git rev-parse 'v0.2.0^{commit}')"
assert_eq "发布后工作区干净" "" "$(cd "$R" && git status --porcelain)"
assert_contains "提示如何推送" "$out" "git push origin main v0.2.0"
assert_eq "章节提取" "### 新增

- 新功能 A" "$(cd "$R" && bash scripts/changelog-section.sh 0.2.0)"

# 3. 各种拒绝情况
R="$TMP/r3"; new_repo "$R"
out="$(release "$R" 0.1.0 --no-push --skip-tests)"; st=$?
assert_eq "版本号不变时拒绝" "1" "$st"; assert_contains "说明需要更大的版本号" "$out" "必须大于当前版本"
out="$(release "$R" 0.0.9 --no-push --skip-tests)"; st=$?
assert_eq "版本号变小时拒绝" "1" "$st"
out="$(release "$R" 1.2 --no-push --skip-tests)"; st=$?
assert_eq "非法版本号拒绝" "1" "$st"; assert_contains "说明版本号格式" "$out" "X.Y.Z"
(cd "$R" && echo x >> a/src/lib.rs)
out="$(release "$R" 0.2.0 --no-push --skip-tests)"; st=$?
assert_eq "工作区不干净时拒绝" "1" "$st"; assert_contains "说明未提交改动" "$out" "未提交"
(cd "$R" && git checkout -q -- a/src/lib.rs && git checkout -q -b other)
out="$(release "$R" 0.2.0 --no-push --skip-tests)"; st=$?
assert_eq "不在 main 时拒绝" "1" "$st"; assert_contains "说明分支要求" "$out" "main"
(cd "$R" && git checkout -q main && git tag v0.3.0)
out="$(release "$R" 0.3.0 --no-push --skip-tests)"; st=$?
assert_eq "标签已存在时拒绝" "1" "$st"; assert_contains "说明标签已存在" "$out" "已存在"

R="$TMP/r4"; new_repo "$R" ""
out="$(release "$R" 0.2.0 --no-push --skip-tests)"; st=$?
assert_eq "未发布为空时拒绝" "1" "$st"; assert_contains "说明更新日志为空" "$out" "未发布"

# 4. 推送：--yes 跳过确认，推送到本地裸仓库
R="$TMP/r5"; new_repo "$R"
git init -q --bare "$TMP/remote.git"
(cd "$R" && git remote add origin "$TMP/remote.git" && git push -q origin main)
out="$(release "$R" 0.2.0 --yes --skip-tests)"; st=$?
assert_status "推送成功" 0 "$st"
assert_eq "远端有标签" "v0.2.0" "$(git --git-dir="$TMP/remote.git" tag)"
assert_eq "远端 main 已更新" "$(cd "$R" && git rev-parse main)" "$(git --git-dir="$TMP/remote.git" rev-parse main)"

# 5. 不带 --yes 且回答 n：不推送
R="$TMP/r6"; new_repo "$R"
git init -q --bare "$TMP/remote6.git"
(cd "$R" && git remote add origin "$TMP/remote6.git" && git push -q origin main)
out="$(cd "$R" && echo n | bash scripts/release.sh 0.2.0 --skip-tests 2>&1)"; st=$?
assert_status "拒绝推送时正常退出" 0 "$st"
assert_eq "远端没有标签" "" "$(git --git-dir="$TMP/remote6.git" tag)"
assert_contains "提示稍后手动推送" "$out" "git push origin main v0.2.0"

finish
