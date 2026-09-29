import { useCallback, useEffect, useRef, useState } from "react"
import { CheckIcon, CopyIcon } from "lucide-react"

import { Button } from "@/components/ui/button"

/** 剪贴板 API 在非安全上下文（http）下不可用，退回 textarea + execCommand。 */
async function copyText(text: string): Promise<boolean> {
  if (navigator.clipboard && window.isSecureContext) {
    try {
      await navigator.clipboard.writeText(text)
      return true
    } catch {
      // 落到下面的兜底实现
    }
  }

  try {
    const area = document.createElement("textarea")
    area.value = text
    area.setAttribute("readonly", "")
    area.style.position = "fixed"
    area.style.opacity = "0"
    document.body.appendChild(area)
    area.select()
    const ok = document.execCommand("copy")
    document.body.removeChild(area)
    return ok
  } catch {
    return false
  }
}

interface InstallCommandProps {
  title: string
  command: string
  note: string
}

/** 安装命令：一行命令 + 复制按钮 + 说明。 */
export function InstallCommand({ title, command, note }: InstallCommandProps) {
  const [copied, setCopied] = useState(false)
  const timer = useRef<number | null>(null)

  useEffect(
    () => () => {
      if (timer.current !== null) window.clearTimeout(timer.current)
    },
    [],
  )

  const onCopy = useCallback(async () => {
    if (!(await copyText(command))) return
    setCopied(true)
    if (timer.current !== null) window.clearTimeout(timer.current)
    timer.current = window.setTimeout(() => setCopied(false), 1600)
  }, [command])

  return (
    <div className="flex flex-col gap-2.5">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="text-sm text-muted-foreground">{title}</p>
        <Button type="button" variant="outline" size="sm" onClick={onCopy}>
          {copied ? (
            <CheckIcon data-icon="inline-start" />
          ) : (
            <CopyIcon data-icon="inline-start" />
          )}
          <span aria-live="polite">{copied ? "已复制" : "复制"}</span>
        </Button>
      </div>
      <pre className="overflow-x-auto rounded-lg border bg-background/60 px-4 py-3.5 font-mono text-[13px]/relaxed whitespace-pre-wrap break-all">
        <code>{command}</code>
      </pre>
      <p className="text-xs/relaxed text-muted-foreground">{note}</p>
    </div>
  )
}
