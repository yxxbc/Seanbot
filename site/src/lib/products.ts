import { RELEASES } from "@/lib/site"

/**
 * 产品形态：CLI（单轮入口）/ TUI（默认界面）/ App（桌面端，未发布）。
 *
 * 三个形态共用同一个 sean 可执行文件，所以安装命令跟着平台走、与产品无关；
 * App 发布后把 share 改成 true、补上 previews/app.png 即可（页面会自动恢复
 * 下载入口与预览图）。
 */
export type ProductId = "cli" | "tui" | "app"

export interface Product {
  id: ProductId
  /** 标签页标题 */
  name: string
  /** 标签页副标题 */
  note: string
  /** 状态徽章文案 */
  pill: string
  /** 有没有可下载的发布包 */
  share: boolean
  statusText: string
  /** 预览图：放在 site/public/previews/ 下的文件名 */
  preview: string
  /** 预览窗口标题栏文案 */
  windowTitle: string
  /** 预览图说明 */
  caption: string
  /** 预览图还没到位时的占位文案 */
  emptyTitle: string
  emptyNote: string
  /** 未发布形态的说明块 */
  wip?: { title: string; text: string; linkText: string }
}

export const PRODUCTS: Record<ProductId, Product> = {
  cli: {
    id: "cli",
    name: "CLI",
    note: "单轮 / 脚本",
    pill: "可用",
    share: true,
    statusText:
      '单轮提问与脚本友好：sean -p "问题" 输出答案就退出，重定向到文件时是原始 Markdown。',
    preview: "previews/cli.png",
    windowTitle: "sean -p — 单轮",
    caption:
      'CLI：sean -p "问题" 跑一轮就退出，适合脚本与 CI；非交互模式下改动类工具默认被拒绝。',
    emptyTitle: "预览图正在补充",
    emptyNote: "截图稍后补上；上面的下载与安装命令现在就能用。",
  },
  tui: {
    id: "tui",
    name: "TUI",
    note: "默认界面",
    pill: "可用",
    share: true,
    statusText: "默认界面：行内 TUI，与 CLI 是同一个 sean 可执行文件。",
    preview: "previews/tui.png",
    windowTitle: "sean — TUI",
    caption:
      "TUI：流式 Markdown、/ 命令浮窗、改动前的确认框、Ctrl+O 转录视图，还有吉祥物环环。",
    emptyTitle: "预览图正在补充",
    emptyNote: "截图稍后补上；上面的下载与安装命令现在就能用。",
  },
  app: {
    id: "app",
    name: "App",
    note: "桌面端",
    pill: "开发中",
    share: false,
    statusText: "桌面端还没有可下载的版本。",
    preview: "previews/app.png",
    windowTitle: "Seanbot — 桌面端",
    caption: "App：桌面端仍在开发，截图稍后补上。",
    emptyTitle: "界面还在开发",
    emptyNote: "桌面端尚未发布，先关注 GitHub Releases。",
    wip: {
      title: "桌面端正在开发",
      text: "目前还没有可下载的版本。想第一时间拿到，可以到 GitHub 关注 Releases，或者在 Issue 里说一句你最想要的能力。",
      linkText: "关注 Releases",
    },
  },
}

export const PRODUCT_IDS: ProductId[] = ["cli", "tui", "app"]

/** 默认展示的形态：TUI 是现在的默认界面。 */
export const DEFAULT_PRODUCT: ProductId = "tui"

export function productOf(id: ProductId): Product {
  return PRODUCTS[id] ?? PRODUCTS[DEFAULT_PRODUCT]
}

/** 未发布形态的说明链接统一指向 Releases。 */
export function wipLink(): string {
  return RELEASES
}
