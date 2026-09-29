# 预览图放这里

官网「产品预览」区域会按当前选中的产品，尝试加载本目录下的固定文件名：

| 文件 | 对应产品 | 建议尺寸 |
| --- | --- | --- |
| `cli.png` | CLI（逐行对话） | 1600×1000 |
| `tui.png` | TUI（全屏界面） | 1600×1000 |
| `app.png` | App（桌面端） | 1600×1000 |

放进去、刷新页面就会自动显示；文件不存在时页面显示「预览图正在补充」占位，
**不需要改任何代码**。

## 约定

- PNG 优先，单张控制在 1 MiB 以内——仓库的 pre-commit 钩子会拒绝超过 1 MiB 的文件。
  超了就压一下（例如 `pngquant --quality 70-90 cli.png`）。
- 内容建议：真实终端截图，带一点窗口背景，保证深色主题下边缘不糊。
- 想换文件名或格式（webp 等）：改 `site/assets/app.js` 里 `PRODUCTS[...].preview`。

## 还要补的素材

- `site/assets/og.png`：分享卡片，**已就绪**（1200×630，由 `scripts/make-og.py` 生成）。
  改文案后重跑一次：`python3 scripts/make-og.py`（需要 Python 3 + Pillow）。
- `site/previews/cli.png`、`tui.png`、`app.png`：真实截图还没补，页面会显示「预览图正在补充」。
- `site/assets/version.json`：不用手改，由 `scripts/sync-site.sh` 在发布（`scripts/release.sh`）
  与部署（`.github/workflows/pages.yml`）时自动写入当前版本与发布日期。
