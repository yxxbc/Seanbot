import type { ReactNode } from "react"

import { CodeChip } from "@/components/site/code-chip"
import { SectionHead } from "@/components/site/section-head"
import { Badge } from "@/components/ui/badge"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import { PRODUCTS, type ProductId } from "@/lib/products"

/** 三样东西的进度：状态徽章直接取 PRODUCTS 里的 pill，避免两处说法不一致。 */
const STATUS: { id: ProductId; body: ReactNode }[] = [
  {
    id: "cli",
    body: (
      <>
        单轮入口：<CodeChip>sean -p "问题"</CodeChip>
        输出答案就退出，适合脚本与 CI；重定向到文件时输出原始 Markdown。五个构建目标（macOS / Linux /
        Windows）都能装，<CodeChip>sean update</CodeChip> 一键升级。
      </>
    ),
  },
  {
    id: "tui",
    body: (
      <>
        默认界面：行内 TUI（ratatui）。助手回复流式 Markdown 写进终端滚动区，<CodeChip>/</CodeChip>
        弹出命令浮窗，改动类工具有确认框，<CodeChip>Ctrl+O</CodeChip>{" "}
        打开转录视图，还有吉祥物“环环”。与 CLI 同一个 <CodeChip>sean</CodeChip>，装完直接用。
      </>
    ),
  },
  {
    id: "app",
    body: <>桌面端还在做，没有可下载的版本。欢迎在 Issue 里提需求。</>,
  },
]

export function Status() {
  return (
    <section id="status" className="mx-auto max-w-6xl px-6 pt-2 pb-16 sm:pb-20">
      <SectionHead eyebrow="项目状态" title="三样东西，进度不一样" />

      <div className="mt-8 grid gap-3 md:grid-cols-3">
        {STATUS.map((item) => (
          <Card key={item.id} className="gap-3">
            <CardHeader>
              <div className="flex items-center gap-3">
                <Badge variant={PRODUCTS[item.id].share ? "secondary" : "outline"}>
                  {PRODUCTS[item.id].pill}
                </Badge>
                <CardTitle>{PRODUCTS[item.id].name}</CardTitle>
              </div>
            </CardHeader>
            <CardContent className="text-sm/relaxed text-muted-foreground">
              {item.body}
            </CardContent>
          </Card>
        ))}
      </div>
    </section>
  )
}
