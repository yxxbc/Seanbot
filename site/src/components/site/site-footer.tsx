import { LogoMark } from "@/components/site/brand-icons"
import { Separator } from "@/components/ui/separator"
import { CHANGELOG, ISSUES, LICENSE, REPO } from "@/lib/site"

const LINKS = [
  { href: REPO, label: "GitHub" },
  { href: ISSUES, label: "问题反馈" },
  { href: CHANGELOG, label: "更新日志" },
  { href: LICENSE, label: "Apache-2.0 协议" },
]

export function SiteFooter() {
  return (
    <footer className="mt-12">
      <Separator />
      <div className="mx-auto flex max-w-6xl flex-wrap items-center justify-between gap-6 px-6 py-9">
        <div className="flex items-center gap-3.5">
          <LogoMark className="size-10" />
          <div>
            <p className="font-semibold tracking-tight">Seanbot</p>
            <p className="text-sm text-muted-foreground">你的全能代理 · Made with 🦀 Rust</p>
          </div>
        </div>

        <nav className="flex flex-wrap gap-5 text-sm text-muted-foreground" aria-label="相关链接">
          {LINKS.map((link) => (
            <a key={link.href} className="transition-colors hover:text-foreground" href={link.href}>
              {link.label}
            </a>
          ))}
        </nav>
      </div>
    </footer>
  )
}
