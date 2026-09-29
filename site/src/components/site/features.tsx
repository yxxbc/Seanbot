import type { ReactNode } from "react"
import { ArrowRightIcon } from "lucide-react"

import { CodeChip } from "@/components/site/code-chip"
import { SectionHead } from "@/components/site/section-head"
import { Badge } from "@/components/ui/badge"
import { buttonVariants } from "@/components/ui/button"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import { README } from "@/lib/site"
import { cn } from "@/lib/utils"

const FEATURES: { num: string; tag: string; title: string; body: ReactNode }[] = [
  {
    num: "01",
    tag: "TOOLS",
    title: "工具箱够用",
    body: (
      <>
        内核 <CodeChip>read</CodeChip> <CodeChip>edit</CodeChip> <CodeChip>bash</CodeChip>{" "}
        <CodeChip>search</CodeChip> <CodeChip>perceive</CodeChip>；联网 <CodeChip>web_search</CodeChip>{" "}
        <CodeChip>web_fetch</CodeChip>；知识库 <CodeChip>kb_*</CodeChip>；技能{" "}
        <CodeChip>skill</CodeChip>；上限与默认值交给 <CodeChip>config</CodeChip>
        。命令受黑名单约束，上限写在 <CodeChip>config.toml</CodeChip> 里随时可调。
      </>
    ),
  },
  {
    num: "02",
    tag: "SESSION",
    title: "会话随时接着聊",
    body: (
      <>
        <CodeChip>sean -c</CodeChip> 继续上一次，<CodeChip>sean -r</CodeChip>
        从列表里挑。历史逐条落盘，恢复后前缀缓存照旧命中。
      </>
    ),
  },
  {
    num: "03",
    tag: "PERMISSION",
    title: "改动前先问你",
    body: (
      <>
        确认模式下改动类工具逐次征求许可（<CodeChip>edit</CodeChip>、<CodeChip>bash</CodeChip>
        ，以及 <CodeChip>config</CodeChip> 的 set/unset），也能一键 YOLO；黑名单在任何模式下都生效。
      </>
    ),
  },
  {
    num: "04",
    tag: "I18N",
    title: "中文优先",
    body: <>界面、提示词、报错都是中文；命令与工具名保持英文，方便和文档对照。</>,
  },
  {
    num: "05",
    tag: "UPDATE",
    title: "自己升级自己",
    body: (
      <>
        <CodeChip>sean update</CodeChip>
        查最新版、下载对应平台的包、校验 SHA256 后原地替换（Windows 重跑安装命令）；不用回官网手动下载。
      </>
    ),
  },
  {
    num: "06",
    tag: "PROVIDER",
    title: "换厂商只是加一条描述",
    body: <>厂商层数据驱动，内置 DeepSeek；任何 OpenAI 兼容服务加一条描述即可接入。</>,
  },
  {
    num: "07",
    tag: "CONTEXT",
    title: "懂你的项目",
    body: (
      <>
        会话开始自动注入项目指令（<CodeChip>AGENTS.md</CodeChip> / <CodeChip>CLAUDE.md</CodeChip>
        ，从全局到工作目录逐级取，越近的越优先）；走到子目录再按需补该目录的约定。技能放{" "}
        <CodeChip>.seanbot/skills/</CodeChip>，只在需要时读全文，不占常驻上下文。
      </>
    ),
  },
  {
    num: "08",
    tag: "KB",
    title: "自带知识库",
    body: (
      <>
        官方知识库随二进制分发、只读；外置知识库可写。Sean 用 <CodeChip>kb_search</CodeChip>
        查自己的文档、用 <CodeChip>kb_add</CodeChip> 记长期事实；你也能 <CodeChip>sean kb</CodeChip>
        看条目、<CodeChip>sean kb update</CodeChip> 拉最新。
      </>
    ),
  },
]

export function Features() {
  return (
    <section id="features" className="mx-auto max-w-6xl px-6 py-16 sm:py-20">
      <SectionHead
        eyebrow="它到底能做什么"
        title="不只是聊天，它能动手"
        lede="Seanbot 把工具调用串成一条链路：先看现状，再动手改，改完自己验证。每一步都在你的终端里可见、可打断、可拒绝。"
      >
        <a
          className={cn(buttonVariants({ variant: "outline" }), "self-start")}
          href={README}
        >
          看完整说明
          <ArrowRightIcon data-icon="inline-end" />
        </a>
      </SectionHead>

      <div className="mt-9 grid gap-3 sm:grid-cols-2">
        {FEATURES.map((feature) => (
          <Card key={feature.num} className="gap-3">
            <CardHeader>
              <div className="flex items-center gap-2.5">
                <span className="font-mono text-xs text-brand-gold">{feature.num}</span>
                <Badge variant="outline">{feature.tag}</Badge>
              </div>
              <CardTitle>{feature.title}</CardTitle>
            </CardHeader>
            <CardContent className="text-sm/relaxed text-muted-foreground">
              {feature.body}
            </CardContent>
          </Card>
        ))}
      </div>
    </section>
  )
}
