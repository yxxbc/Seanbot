// @ts-check

import tailwindcss from "@tailwindcss/vite"
import { defineConfig } from "astro/config"
import react from "@astrojs/react"

// 官网是纯静态站点：`npm run build` 产出 dist/，由 .github/workflows/pages.yml
// 上传到 GitHub Pages（https://yxxbc.github.io/Seanbot/）。
// base 必须和仓库名一致，否则线上资源的路径会 404；本地预览请访问
// http://localhost:4321/Seanbot/（astro dev 会自动带上 base）。
export default defineConfig({
  site: "https://yxxbc.github.io",
  base: "/Seanbot/",
  vite: {
    plugins: [tailwindcss()],
  },
  integrations: [react()],
})
