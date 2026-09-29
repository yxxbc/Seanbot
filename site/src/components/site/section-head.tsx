import type { ReactNode } from "react"

import { cn } from "@/lib/utils"

interface SectionHeadProps {
  /** 等宽小字，例如 Agent loop */
  eyebrow: string
  title: ReactNode
  lede?: ReactNode
  /** 标题下方额外内容，例如「看完整说明」 */
  children?: ReactNode
  className?: string
}

/** 区块标题：eyebrow + h2 + 一句说明。所有区块都长这样，改一处就够。 */
export function SectionHead({ eyebrow, title, lede, children, className }: SectionHeadProps) {
  return (
    <div className={cn("flex max-w-2xl flex-col gap-3", className)}>
      <p className="font-mono text-xs tracking-[0.16em] text-brand-gold">{eyebrow}</p>
      <h2 className="text-2xl font-semibold tracking-tight text-balance sm:text-3xl">{title}</h2>
      {lede ? (
        <p className="text-sm/relaxed text-pretty text-muted-foreground sm:text-base/relaxed">
          {lede}
        </p>
      ) : null}
      {children}
    </div>
  )
}
