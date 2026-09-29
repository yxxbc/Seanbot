#!/usr/bin/env bash
# 站点自检：版本号跟仓库一致、页面引用的素材都在、分享卡片是绝对地址。
set -uo pipefail
. "$(dirname "$0")/lib.sh"

SITE="$ROOT/site"
version="$(bash "$ROOT/scripts/cargo-version.sh" "$ROOT/Cargo.toml")"
json="$(cat "$SITE/assets/version.json" 2>/dev/null || echo '')"
html="$(cat "$SITE/index.html")"

echo "site"

# 版本文件由 scripts/sync-site.sh 生成，必须与工作区版本一致（不一致就是忘了同步）
assert_contains "version.json 与 Cargo.toml 版本一致" "$json" "\"version\": \"$version\""
assert_contains "version.json 带 tag" "$json" "\"tag\": \"v$version\""
assert_contains "version.json 带发布日期" "$json" '"released"'

# 页面引用的本地素材都要在
for f in index.html assets/style.css assets/app.js assets/icon.svg assets/og.png previews/README.md robots.txt sitemap.xml; do
  [ -f "$SITE/$f" ] && pass "站点包含 $f" || fail "站点缺少 $f"
done

# 分享卡片必须是绝对地址（相对路径在多数平台不生效），且不再指向 SVG
assert_contains "og:image 是绝对地址" "$html" \
  'property="og:image" content="https://yxxbc.github.io/Seanbot/assets/og.png"'
assert_contains "og:image 尺寸已声明" "$html" 'property="og:image:width" content="1200"'
assert_contains "有 twitter 大卡片" "$html" 'name="twitter:card" content="summary_large_image"'
assert_contains "有 canonical" "$html" 'rel="canonical" href="https://yxxbc.github.io/Seanbot/"'
assert_not_contains "og:image 不再指向 SVG" "$html" 'property="og:image" content="assets/icon.svg"'

# app.js 依赖的 id 必须在 HTML 里（手改 HTML 时别漏钩子）
for id in status-pill status-text version-pill download-btn install-cmd copy-btn wip-title wip-text wip-link preview-img placeholder; do
  assert_contains "HTML 有 #$id" "$html" "id=\"$id\""
done

# 版本徽章的元素与样式都在
assert_contains "版本徽章有样式" "$(cat "$SITE/assets/style.css")" '.pill--ver'
assert_contains "app.js 会读 version.json" "$(cat "$SITE/assets/app.js")" 'assets/version.json'

finish
