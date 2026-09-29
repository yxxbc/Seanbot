import type { ReactNode } from "react"

import { CodeChip } from "@/components/site/code-chip"
import { SectionHead } from "@/components/site/section-head"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"

/** Agent 循环的五步。箭头是装饰，窄屏折行时由 CSS 隐藏。 */
const STEPS: { num: string; title: string; body: ReactNode }[] = [
  {
    num: "01",
    title: "理解",
    body: (
      <>
        读懂目标，并自动带上项目指令（
        <CodeChip>AGENTS.md</CodeChip> / <CodeChip>CLAUDE.md</CodeChip>
        ，越靠近工作目录越优先）。
      </>
    ),
  },
  {
    num: "02",
    title: "读取",
    body: (
      <>
        <CodeChip>read</CodeChip> 读文件、<CodeChip>search</CodeChip> 搜代码、
        <CodeChip>kb_search</CodeChip> 查它自带的知识库。
      </>
    ),
  },
  {
    num: "03",
    title: "行动",
    body: (
      <>
        <CodeChip>edit</CodeChip> 改文件、<CodeChip>bash</CodeChip> 跑命令；要联网就{" "}
        <CodeChip>web_search</CodeChip> / <CodeChip>web_fetch</CodeChip>。
      </>
    ),
  },
  {
    num: "04",
    title: "验证",
    body: <>跑测试、看输出；结果不对就回到上一步重来，而不是把问题留给你。</>,
  },
  {
    num: "05",
    title: "完成",
    body: <>汇总改了什么、结果如何，把结论和改动一起交回你手上。</>,
  },
]

export function AgentLoop() {
  return (
    <section id="workflow" className="mx-auto max-w-6xl px-6 py-16 sm:py-20">
      <SectionHead
        eyebrow="Agent loop"
        title="从一句话，到事情完成"
        lede="每一次行动都经过同一套循环：工具不是散落在界面里的命令，而是它随时可以调用、也能被你拦下的能力。"
      />

      <ol className="mt-9 grid gap-3 sm:grid-cols-2 lg:grid-cols-5">
        {STEPS.map((step, index) => (
          <li key={step.num} className="relative">
            <Card size="sm" className="h-full gap-3">
              <CardHeader>
                <span className="font-mono text-xs text-brand-gold">{step.num}</span>
                <CardTitle>{step.title}</CardTitle>
              </CardHeader>
              <CardContent className="text-sm/relaxed text-muted-foreground">
                {step.body}
              </CardContent>
            </Card>
            {index < STEPS.length - 1 ? (
              <span
                className="absolute top-1/2 -right-3 hidden -translate-y-1/2 text-muted-foreground lg:block"
                aria-hidden="true"
              >
                →
              </span>
            ) : null}
          </li>
        ))}
      </ol>
    </section>
  )
}
