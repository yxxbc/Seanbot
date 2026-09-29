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

- `site/assets/og.png`：1200×630 的社交分享卡片，接好后把 `site/index.html` 里
  `og:image` 的 `assets/icon.svg` 换成 `assets/og.png`。
