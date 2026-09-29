# 官网（Astro + shadcn/ui）

<https://yxxbc.github.io/Seanbot/> 的源码。页面在**构建时**生成静态 HTML：只有「下载」面板
与「产品预览」两个 island 会在浏览器里水合，其余内容都是纯 HTML（SEO 与首屏优先）。

## 本地开发

```bash
cd site
npm install          # 首次
npm run dev          # http://localhost:4321/Seanbot/ —— base 是仓库名，别漏了
npm run build        # 产出 dist/，与 CI 上跑的是同一条命令
npm run preview      # 预览构建产物
npm run typecheck    # astro check
```

## 结构

| 路径 | 作用 |
| --- | --- |
| `astro.config.mjs` | `base: "/Seanbot/"` 必须与仓库名一致，否则线上资源 404 |
| `src/pages/index.astro` | 页面顺序与锚点（工作方式 / 下载 / 预览 / 功能） |
| `src/components/site/` | 页面组件；`download-panel` 与 `product-preview` 是仅有的两个 island |
| `src/components/ui/` | shadcn 组件源码，用 CLI 加：`npx shadcn@latest add <name>` |
| `src/lib/products.ts` | 产品形态（CLI / TUI / App）的文案与可用性 |
| `src/lib/targets.ts` | 平台 / 架构 → 发布产物，以及系统识别 |
| `src/lib/product-store.ts` | 两个 island 之间共享「当前选中的形态」 |
| `src/styles/global.css` | 主题 token（品牌深色为默认） |
| `public/` | 原样拷进 `dist/`：图标、分享卡片、robots.txt、sitemap.xml、预览图 |

## 主题

默认深色（`base.astro` 给 `<html>` 挂了 `class="dark"`），配色取自品牌图标：近黑 `#0E0D10`、
奶白 `#F4E9D8`、金 `#E6B85C`、珊瑚 `#D95F4B`。改配色只动 `src/styles/global.css` 里的 token；
组件里统一用语义 token（`bg-card`、`text-muted-foreground`…），不要写死颜色。字体只用系统字体
（不请求网络字体，中文走 PingFang / 微软雅黑 / Noto）。

## 素材（都不用改代码）

- `public/previews/cli.png`、`tui.png`、`app.png`：产品预览截图，放进去刷新就显示
  （加载失败时显示「预览图正在补充」占位）。建议 1600×1000、单张 < 1 MiB——仓库的
  pre-commit 钩子会拒绝超过 1 MiB 的文件。
- `public/assets/og.png`：分享卡片（1200×630），改文案后重跑 `python3 scripts/make-og.py`。
- `public/assets/version.json`：不用手改，由 `scripts/sync-site.sh` 在发版
  （`scripts/release.sh`）与部署（`.github/workflows/pages.yml`）时写入当前版本与发布日期。

## 部署与自检

`.github/workflows/pages.yml` 在推送 main 且改动 `site/**` 时跑 `npm ci` → `npm run build`，
把 `site/dist` 上传到 GitHub Pages；`dist/` 与 `node_modules/` 都不入库。
`scripts/tests/site_test.sh` 只查源码与配置（不跑 npm），守住工程结构、`base`、
分享卡片绝对地址、版本徽章与预览图这些关键钩子。
