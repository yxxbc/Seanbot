import type { ReactNode } from "react"

/** 行内代码：正文里的命令、工具名、文件名都用它，保证字体与底色一致。 */
export function CodeChip({ children }: { children: ReactNode }) {
  return (
    <code className="rounded-md border bg-muted/50 px-1.5 py-0.5 font-mono text-[0.85em] whitespace-nowrap">
      {children}
    </code>
  )
}
