import { DownloadIcon } from "lucide-react"

import { TerminalMock } from "@/components/site/terminal-mock"
import { Badge } from "@/components/ui/badge"
import { buttonVariants } from "@/components/ui/button"

const META = ["Rust 1.85+", "macOS · Linux · Windows", "Apache-2.0 开源"]

export function Hero() {
  return (
    <>
      {/* 首屏 */}
      <section className="mx-auto max-w-6xl px-6 pt-16 pb-12 sm:pt-22">
        <div className="grid items-center gap-9 lg:grid-cols-2 lg:gap-14">
          <div className="flex animate-in flex-col gap-5 duration-500 fade-in slide-in-from-bottom-2 motion-reduce:animate-none">
            <p className="font-mono text-xs tracking-[0.16em] text-brand-gold">
              Rust · 中文界面 · DeepSeek
            </p>
            <h1 className="text-3xl font-semibold tracking-tight text-balance sm:text-4xl lg:text-5xl">
              你的全能代理。
              <br />
              <span className="text-[0.62em] font-medium text-muted-foreground">
                说清楚你要什么，剩下的交给它。
              </span>
            </h1>
            <p className="max-w-2xl text-base/relaxed text-pretty text-muted-foreground">
              Seanbot
              是跑在终端里的 AI 代理：读文件、改代码、跑命令、联网查资料，用中文对话，把事办完而不只是聊天。支持
              macOS、Linux 与 Windows。
            </p>
            <div className="flex flex-wrap gap-3">
              <a className={buttonVariants({ size: "lg" })} href="#download">
                <DownloadIcon data-icon="inline-start" />
                一行命令安装
              </a>
              <a className={buttonVariants({ variant: "ghost", size: "lg" })} href="#preview">
                看看长什么样
              </a>
            </div>
            <ul className="flex flex-wrap gap-2">
              {META.map((item) => (
                <li key={item}>
                  <Badge variant="outline">{item}</Badge>
                </li>
              ))}
              <li>
                <Badge variant="outline">
                  装完 <span className="font-mono">sean update</span> 一键升级
                </Badge>
              </li>
            </ul>
          </div>

          <div className="animate-in duration-700 fade-in slide-in-from-bottom-2 motion-reduce:animate-none">
            <TerminalMock />
          </div>
        </div>
      </section>

      {/* 宣言：它不是一个聊天窗口 */}
      <section className="mx-auto max-w-6xl px-6 pt-3 pb-2">
        <div className="flex max-w-4xl flex-col gap-4">
          <p className="font-mono text-xs tracking-[0.16em] text-brand-gold">Not a chatbot</p>
          <h2 className="text-3xl font-semibold tracking-tight text-balance sm:text-4xl lg:text-[3.05rem] lg:leading-[1.14]">
            不是聊天窗口。
            <br />
            <span className="text-muted-foreground">是一个可以工作的 Agent。</span>
          </h2>
          <p className="max-w-3xl text-base/relaxed text-pretty text-muted-foreground sm:text-lg/relaxed">
            Seanbot
            把模型、工具、权限和项目上下文串成一条链路。你只说目标，剩下的由它自己决定读什么、改什么、跑什么，以及什么时候停下来问你。
          </p>
        </div>
      </section>
    </>
  )
}
