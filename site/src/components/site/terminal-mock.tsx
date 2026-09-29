import { WindowFrame } from "@/components/site/window-frame"

/**
 * 首屏右侧的终端示意图：纯 HTML/CSS 画的，不是截图（真实截图放在
 * site/public/previews/，由「产品预览」区展示）。
 *
 * 不写版本号：发版频繁，写死的数字很快就过期，真实版本由程序自己打印。
 */
const TOOL_LINES = [
  {
    tool: "Read(crates/seanbot-core/src/tools/edit.rs)",
    note: "· 读取 180 行",
  },
  { tool: "Edit(.../tools/edit.rs)", note: "· +42 -18 行" },
  { tool: "Bash(cargo test -p seanbot-core)", note: "· 32 passed" },
]

export function TerminalMock() {
  return (
    <WindowFrame
      title="sean — ~/Projects/Seanbot"
      bodyClassName="overflow-x-auto px-4.5 py-4 font-mono text-xs/relaxed sm:text-[12.5px]"
    >
      <div
        role="img"
        aria-label="Seanbot 会话示意：读文件、改代码、跑测试，完成后回到提示符"
        className="w-max"
      >
        <p className="whitespace-pre">
          <span className="text-muted-foreground">$ </span>
          <span className="text-brand-gold">sean</span>
        </p>

        <div className="my-2.5 flex flex-col gap-1 rounded-lg border bg-background/60 px-3 py-2.5">
          <p className="text-muted-foreground">Seanbot · deepseek-flash · ~/Projects/Seanbot</p>
          <p className="whitespace-pre text-foreground">
            <span className="text-brand-gold">› </span>
            把 edit 报错改短一点，再加个「第 N 处」参数
          </p>
        </div>

        <p className="whitespace-pre text-brand-coral">✻ Thought for 1.2s</p>
        {TOOL_LINES.map((line) => (
          <p key={line.tool} className="whitespace-pre">
            <span className="font-bold text-brand-gold">✓ </span>
            {line.tool} <span className="text-muted-foreground">{line.note}</span>
          </p>
        ))}
        <p className="whitespace-pre text-muted-foreground">↑12.4k（缓存 9.1k） ↓318 · 4 步</p>
        <p className="whitespace-pre">
          <span className="text-muted-foreground">$ </span>
          <span
            className="inline-block h-3.5 w-2 translate-y-0.5 animate-caret-blink bg-brand-gold"
            aria-hidden="true"
          />
        </p>
      </div>
    </WindowFrame>
  )
}
