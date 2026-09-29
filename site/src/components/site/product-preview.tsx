import { useEffect, useState } from "react"

import { LogoMark } from "@/components/site/brand-icons"
import { WindowFrame } from "@/components/site/window-frame"
import { Badge } from "@/components/ui/badge"
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "@/components/ui/empty"
import { useSelectedProduct } from "@/lib/product-store"
import { productOf } from "@/lib/products"
import { assetUrl } from "@/lib/site"

/**
 * 产品预览：跟着「下载」面板里选中的形态走。
 *
 * 截图放在 site/public/previews/ 下（cli.png / tui.png / app.png）。先探一次能否
 * 加载：素材还没到位就显示 Empty 占位，而不是破图——补图不用改代码。
 */
export function ProductPreview() {
  const product = productOf(useSelectedProduct())
  const src = assetUrl(product.preview)
  const [loaded, setLoaded] = useState(false)

  useEffect(() => {
    setLoaded(false)
    const probe = new Image()
    probe.onload = () => setLoaded(true)
    probe.src = src
    return () => {
      probe.onload = null
    }
  }, [src])

  return (
    <WindowFrame
      title={product.windowTitle}
      aside={
        <Badge variant={product.share ? "secondary" : "outline"}>{product.pill}</Badge>
      }
      caption={product.caption}
      bodyClassName="grid min-h-56 place-items-center p-6"
    >
      {loaded ? (
        <img src={src} alt={`${product.name} 界面预览`} className="w-full" />
      ) : (
        <Empty>
          <EmptyHeader>
            <EmptyMedia variant="icon">
              <LogoMark className="size-5" />
            </EmptyMedia>
            <EmptyTitle>{product.emptyTitle}</EmptyTitle>
            <EmptyDescription>{product.emptyNote}</EmptyDescription>
          </EmptyHeader>
        </Empty>
      )}
    </WindowFrame>
  )
}
