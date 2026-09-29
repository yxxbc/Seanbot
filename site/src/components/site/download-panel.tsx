import { useCallback, useEffect, useMemo, useState } from "react"

import { PanelBody } from "@/components/site/download-panel-body"
import { Card } from "@/components/ui/card"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { selectProduct, useSelectedProduct } from "@/lib/product-store"
import { PRODUCTS, PRODUCT_IDS, type ProductId } from "@/lib/products"
import { assetUrl } from "@/lib/site"
import {
  archOf,
  artifactName,
  detectArch,
  detectOS,
  isMobile,
  platformOf,
  type ArchId,
  type OsId,
} from "@/lib/targets"

interface VersionInfo {
  version: string
  released?: string
}

/**
 * 「安装与升级」面板。
 *
 * 只有这一块（以及产品预览）需要水合：静态 HTML 已经能读，交互是渐进增强——
 * 识别系统与架构、切换形态、复制安装命令、按平台给出下载产物。
 */
export function DownloadPanel() {
  const product = useSelectedProduct()
  const [os, setOs] = useState<OsId>("macos")
  const [arch, setArch] = useState<ArchId>(platformOf("macos").defaultArch)
  const [detected, setDetected] = useState(false)
  const [mobile, setMobile] = useState(false)
  const [version, setVersion] = useState<VersionInfo | null>(null)

  /* 系统与架构识别只在浏览器里跑：服务端渲染出与客户端不同的首帧会被水合掉。
     架构是异步问出来的，拿到后只更新与架构有关的部分。 */
  useEffect(() => {
    const detectedOs = detectOS(navigator)
    setMobile(isMobile(navigator))
    if (detectedOs) {
      setOs(detectedOs)
      setArch(platformOf(detectedOs).defaultArch)
      setDetected(true)
    }

    let alive = true
    detectArch(navigator).then((detectedArch) => {
      if (!alive || !detectedArch) return
      const target = platformOf(detectedOs ?? "macos")
      if (target.archs.some((item) => item.id === detectedArch)) setArch(detectedArch)
    })
    return () => {
      alive = false
    }
  }, [])

  /* 版本徽章：site/public/assets/version.json 由 scripts/sync-site.sh 在发布与
     部署时写入，用户不必点进 GitHub 才知道当前版本；文件缺失时安静隐藏。 */
  useEffect(() => {
    let alive = true
    fetch(assetUrl("assets/version.json"), { cache: "no-cache" })
      .then((response) => (response.ok ? response.json() : null))
      .then((data: VersionInfo | null) => {
        if (alive && data?.version) setVersion(data)
      })
      .catch(() => {
        /* 本地打开或文件缺失：不显示版本徽章 */
      })
    return () => {
      alive = false
    }
  }, [])

  const onOsChange = useCallback((next: OsId) => {
    setOs(next)
    setArch(platformOf(next).defaultArch)
    setDetected(true)
  }, [])

  const onArchChange = useCallback((next: ArchId) => setArch(next), [])

  const platform = platformOf(os)
  const selectedArch = archOf(os, arch)
  const file = artifactName(os, arch)

  const detectNote = useMemo(() => {
    if (detected) {
      const base = `已识别：${platform.detectNote} · ${selectedArch.label}`
      return mobile ? `${base}（移动端浏览器无法运行 Seanbot，请在电脑上下载）` : base
    }
    if (mobile) {
      return `移动端无法运行 Seanbot，请在电脑上打开本页（下面按 ${platform.label} 预选）`
    }
    return `没认出你的系统，先按 ${platform.label} 显示，手动点一下更准`
  }, [detected, mobile, platform, selectedArch])

  return (
    <Card className="gap-0 p-2">
      <Tabs
        value={product}
        onValueChange={(value) => selectProduct(value as ProductId)}
        className="gap-3"
      >
        {/* activateOnFocus：方向键直接切形态（和旧版页面一致，tablist 的自动激活模式） */}
        <TabsList className="h-auto w-full gap-1" activateOnFocus>
          {PRODUCT_IDS.map((id) => (
            <TabsTrigger
              key={id}
              value={id}
              className="h-auto flex-col items-start gap-0.5 py-2.5"
            >
              <span className="text-sm font-medium">{PRODUCTS[id].name}</span>
              <span className="hidden text-xs text-muted-foreground sm:block">
                {PRODUCTS[id].note}
              </span>
            </TabsTrigger>
          ))}
        </TabsList>

        {PRODUCT_IDS.map((id) => (
          <TabsContent key={id} value={id} className="px-2.5 pb-1.5">
            <PanelBody
              product={PRODUCTS[id]}
              platform={platform}
              selectedArch={selectedArch}
              os={os}
              onOsChange={onOsChange}
              onArchChange={onArchChange}
              detectNote={detectNote}
              file={file}
              version={version}
            />
          </TabsContent>
        ))}
      </Tabs>
    </Card>
  )
}
