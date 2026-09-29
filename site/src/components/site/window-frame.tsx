import type { ReactNode } from "react"

import { cn } from "@/lib/utils"

/**
 * 窗口示意图的外框：英雄区的终端和「产品预览」共用同一套外壳。
 * 只是装饰（三个圆点不是真实窗口控件），所以要的是 figure + figcaption 的语义。
 */
interface WindowFrameProps {
  /** 标题栏文案（等宽字体显示） */
  title: ReactNode
  /** 标题栏右侧的东西，例如状态徽章 */
  aside?: ReactNode
  /** 图注：一个 figure 只放一个 figcaption，所以标题用普通 span */
  caption?: ReactNode
  className?: string
  bodyClassName?: string
  children: ReactNode
}

export function WindowFrame({
  title,
  aside,
  caption,
  className,
  bodyClassName,
  children,
}: WindowFrameProps) {
  return (
    <figure
      className={cn("overflow-hidden rounded-xl border bg-card shadow-2xl", className)}
    >
      <div className="flex items-center gap-2.5 border-b bg-muted/40 px-3.5 py-2.5">
        <span className="flex gap-1.5" aria-hidden="true">
          <span className="size-2.5 rounded-full bg-border" />
          <span className="size-2.5 rounded-full bg-border" />
          <span className="size-2.5 rounded-full bg-border" />
        </span>
        <span className="truncate font-mono text-xs text-muted-foreground">{title}</span>
        {aside ? <div className="ml-auto flex items-center gap-2">{aside}</div> : null}
      </div>
      <div className={bodyClassName}>{children}</div>
      {caption ? (
        <figcaption className="border-t px-4 py-3 text-xs/relaxed text-muted-foreground">
          {caption}
        </figcaption>
      ) : null}
    </figure>
  )
}
