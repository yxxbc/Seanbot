#!/usr/bin/env bash
# 站点自检：版本号跟仓库一致、Astro 工程结构齐全、构建产物不入库、
# 分享卡片是绝对地址，以及版本徽章 / 预览图这些关键钩子还在。
# 这里只看源码与配置，不跑 npm（CI 的三个平台都能直接跑这个测试）。
set -uo pipefail
. "$(dirname "$0")/lib.sh"

SITE="$ROOT/site"
version="$(bash "$ROOT/scripts/cargo-version.sh" "$ROOT/Cargo.toml")"
json="$(cat "$SITE/public/assets/version.json" 2>/dev/null || echo '')"
base="$(cat "$SITE/src/layouts/base.astro" 2>/dev/null || echo '')"
panel="$(cat "$SITE/src/components/site/download-panel.tsx" 2>/dev/null || echo '')"
body="$(cat "$SITE/src/components/site/download-panel-body.tsx" 2>/dev/null || echo '')"
preview="$(cat "$SITE/src/components/site/product-preview.tsx" 2>/dev/null || echo '')"
products="$(cat "$SITE/src/lib/products.ts" 2>/dev/null || echo '')"
config="$(cat "$SITE/astro.config.mjs" 2>/dev/null || echo '')"
ignore="$(cat "$ROOT/.gitignore" 2>/dev/null || echo '')"

echo "site"

# 版本文件由 scripts/sync-site.sh 生成，必须与工作区版本一致（不一致就是忘了同步）
assert_contains "version.json 与 Cargo.toml 版本一致" "$json" "\"version\": \"$version\""
assert_contains "version.json 带 tag" "$json" "\"tag\": \"v$version\""
assert_contains "version.json 带发布日期" "$json" '"released"'

# Astro 工程结构（只查源码，node_modules 与 dist 不入库、也不在这里构建）
for f in package.json astro.config.mjs components.json tsconfig.json \
  src/pages/index.astro src/layouts/base.astro src/styles/global.css \
  src/lib/products.ts src/lib/targets.ts \
  public/assets/icon.svg public/assets/og.png \
  public/previews/.gitkeep public/robots.txt public/sitemap.xml; do
  [ -f "$SITE/$f" ] && pass "站点包含 $f" || fail "站点缺少 $f"
done

# shadcn 组件是加进仓库的源码：少一个页面就编译不过，这里守住页面用到的几个
for c in button badge card tabs separator alert toggle-group empty; do
  [ -f "$SITE/src/components/ui/$c.tsx" ] && pass "shadcn 组件在：$c" || fail "shadcn 组件缺失：$c"
done

# base 必须是仓库名，否则线上资源路径会 404（GitHub Pages 是项目路径）
assert_contains "astro.config.mjs 的 base 是仓库名" "$config" 'base: "/Seanbot/"'

# 依赖与构建产物不入库：线上由 .github/workflows/pages.yml 现构建
assert_contains ".gitignore 忽略站点依赖" "$ignore" "site/node_modules"
assert_contains ".gitignore 忽略构建产物" "$ignore" "site/dist"

# 分享卡片必须是绝对地址（相对路径在多数平台不生效），且不再指向 SVG
assert_contains "og:image 是绝对地址" "$base" \
  'content="https://yxxbc.github.io/Seanbot/assets/og.png"'
assert_contains "og:image 尺寸已声明" "$base" 'property="og:image:width" content="1200"'
assert_contains "有 twitter 大卡片" "$base" 'name="twitter:card" content="summary_large_image"'
assert_contains "有 canonical" "$base" 'const canonical = "https://yxxbc.github.io/Seanbot/"'
assert_not_contains "og:image 不再指向 SVG" "$base" 'content="assets/icon.svg"'

# 两个 island 的关键行为：版本徽章读 version.json，预览图按产品切换、缺图有占位
assert_contains "下载面板会读 version.json" "$panel" 'assets/version.json'
assert_contains "版本徽章文案在" "$body" "最新版本 v"
assert_contains "预览图按产品切换" "$products" 'previews/'
assert_contains "预览图缺失时有占位" "$preview" 'emptyTitle'

finish

