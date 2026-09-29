import { GithubIcon, LogoMark } from "@/components/site/brand-icons"
import { Badge } from "@/components/ui/badge"
import { buttonVariants } from "@/components/ui/button"
import { CHANGELOG, REPO } from "@/lib/site"
import { cn } from "@/lib/utils"

const LINKS = [
  { href: "#workflow", label: "工作方式" },
  { href: "#download", label: "下载" },
  { href: "#preview", label: "预览" },
  { href: "#features", label: "功能" },
  { href: CHANGELOG, label: "更新日志" },
]

export function SiteHeader() {
  return (
    <header className="sticky top-0 z-40 border-b bg-background/75 backdrop-blur-md">
      <div className="mx-auto flex max-w-6xl items-center gap-5 px-6 py-3">
        <a className="flex items-center gap-2.5" href="#top">
          <LogoMark className="size-7.5" />
          <span className="font-semibold tracking-tight">Seanbot</span>
          <Badge variant="destructive">预发布</Badge>
        </a>

        <nav
          className="ml-auto hidden items-center gap-5 text-sm text-muted-foreground md:flex"
          aria-label="页面导航"
        >
          {LINKS.map((link) => (
            <a key={link.href} className="transition-colors hover:text-foreground" href={link.href}>
              {link.label}
            </a>
          ))}
        </nav>

        {/* 链接当按钮用：base 版组件用 render + nativeButton={false} 换成 <a> */}
        <a
          className={cn(buttonVariants({ variant: "outline", size: "sm" }), "ml-auto md:ml-0")}
          href={REPO}
        >
          <GithubIcon data-icon="inline-start" />
          GitHub
        </a>
      </div>
    </header>
  )
}
