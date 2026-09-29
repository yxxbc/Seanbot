import { useSyncExternalStore } from "react"

import { DEFAULT_PRODUCT, type ProductId } from "@/lib/products"

/**
 * 当前选中的产品形态。
 *
 * 「下载」面板与「产品预览」是两个独立的 Astro island：它们不共享 React context，
 * 所以用一个最小的 store 串起来（15 行、零依赖）。服务端渲染时永远返回默认值，
 * 保证首屏 HTML 与客户端首帧一致，不产生水合不一致的警告。
 */
let selected: ProductId = DEFAULT_PRODUCT
const listeners = new Set<() => void>()

export function selectProduct(id: ProductId) {
  if (selected === id) return
  selected = id
  for (const listener of listeners) listener()
}

function subscribe(listener: () => void) {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export function useSelectedProduct(): ProductId {
  return useSyncExternalStore(
    subscribe,
    () => selected,
    () => DEFAULT_PRODUCT,
  )
}
