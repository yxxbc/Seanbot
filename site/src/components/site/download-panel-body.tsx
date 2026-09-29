import { type ComponentType, type SVGProps } from "react"
import { ArrowRightIcon, CheckIcon, DownloadIcon, TriangleAlertIcon } from "lucide-react"

import { AppleIcon, LinuxIcon, WindowsIcon } from "@/components/site/brand-icons"
import { InstallCommand } from "@/components/site/install-command"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Separator } from "@/components/ui/separator"
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group"
import { LATEST_DOWNLOAD, RELEASES } from "@/lib/site"
import {
  needsArchChoice,
  platformOf,
  OS_IDS,
  type Arch,
  type ArchId,
  type OsId,
  type Platform,
} from "@/lib/targets"
import type { Product } from "@/lib/products"

/** 把图标当组件传，而不是字符串 key。 */
const OS_ICONS: Record<OsId, ComponentType<SVGProps<SVGSVGElement>>> = {
  macos: AppleIcon,
  windows: WindowsIcon,
  linux: LinuxIcon,
}

export interface PanelBodyProps {
  product: Product
  platform: Platform
  selectedArch: Arch
  os: OsId
  onOsChange: (os: OsId) => void
  onArchChange: (arch: ArchId) => void
  /** 识别结果说明，由面板算好传进来 */
  detectNote: string
  /** 当前要下载的文件名 */
  file: string
  version: { version: string; released?: string } | null
}

/**
 * 每个形态一套内容：未发布的形态（App）只留一句说明与关注入口，
 * 已发布的形态给系统 / 架构选择、一行安装命令与手动下载入口。
 */
export function PanelBody({
  product,
  platform,
  selectedArch,
  os,
  onOsChange,
  onArchChange,
  detectNote,
  file,
  version,
}: PanelBodyProps) {
  return (
    <div className="flex flex-col gap-5">
      <div className="flex flex-wrap items-center gap-3">
        <Badge variant={product.share ? "secondary" : "outline"}>
          {product.share ? <CheckIcon data-icon="inline-start" /> : null}
          {product.pill}
        </Badge>
        <p className="text-sm text-muted-foreground">{product.statusText}</p>
        {version ? (
          <Badge
            variant="outline"
            className="ml-auto"
            title={version.released ? `发布于 ${version.released}` : undefined}
          >
            最新版本 v{version.version}
          </Badge>
        ) : null}
      </div>

      {product.share ? (
        <>
          <div className="flex flex-wrap items-center gap-x-4 gap-y-3">
            <ToggleGroup
              variant="outline"
              size="lg"
              value={[os]}
              aria-label="操作系统"
              onValueChange={(next) => {
                const picked = next[0] as OsId | undefined
                if (picked) onOsChange(picked)
              }}
            >
              {OS_IDS.map((id) => {
                const Icon = OS_ICONS[id]
                return (
                  <ToggleGroupItem key={id} value={id}>
                    <Icon data-icon="inline-start" />
                    {platformOf(id).label}
                  </ToggleGroupItem>
                )
              })}
            </ToggleGroup>
            <p
              className="font-mono text-xs text-muted-foreground"
              role="status"
              aria-live="polite"
            >
              {detectNote}
            </p>
          </div>

          {needsArchChoice(os) ? (
            <div className="flex flex-wrap items-center gap-3">
              <span className="font-mono text-xs text-muted-foreground">架构</span>
              <ToggleGroup
                variant="outline"
                size="sm"
                value={[selectedArch.id]}
                aria-label="CPU 架构"
                onValueChange={(next) => {
                  const picked = next[0] as ArchId | undefined
                  if (picked) onArchChange(picked)
                }}
              >
                {platform.archs.map((option) => (
                  <ToggleGroupItem key={option.id} value={option.id}>
                    {option.label}
                  </ToggleGroupItem>
                ))}
              </ToggleGroup>
            </div>
          ) : null}

          <InstallCommand
            title={
              os === "windows" ? "一行命令安装（推荐 · PowerShell）" : "一行命令安装（推荐）"
            }
            command={platform.install}
            note={platform.installNote}
          />

          <div>
            <Separator />
            <div className="mt-4 flex flex-wrap items-center gap-3">
              <Button
                variant="outline"
                render={<a href={LATEST_DOWNLOAD + file} />}
                nativeButton={false}
              >
                <DownloadIcon data-icon="inline-start" />
                手动下载 {platform.label}（{selectedArch.label}）
              </Button>
              <Button variant="ghost" render={<a href={RELEASES} />} nativeButton={false}>
                所有版本与校验和
                <ArrowRightIcon data-icon="inline-end" />
              </Button>
            </div>
            <p className="mt-3 font-mono text-xs/relaxed text-muted-foreground">
              {file} · 解包即用；校验和见 Release 里的 SHA256SUMS
            </p>
          </div>
        </>
      ) : product.wip ? (
        <Alert className="p-4">
          <TriangleAlertIcon />
          <AlertTitle>{product.wip.title}</AlertTitle>
          <AlertDescription className="mt-1 flex flex-col items-start gap-3">
            <p>{product.wip.text}</p>
            <Button
              variant="outline"
              size="sm"
              render={<a href={RELEASES} />}
              nativeButton={false}
            >
              {product.wip.linkText}
              <ArrowRightIcon data-icon="inline-end" />
            </Button>
          </AlertDescription>
        </Alert>
      ) : null}
    </div>
  )
}
