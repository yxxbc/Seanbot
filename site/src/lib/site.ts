/**
 * 仓库地址、发布地址与安装命令。
 *
 * 页面里所有指向 GitHub 的链接都从这里取：换仓库名或换安装方式时只改这一处，
 * 不用在组件里到处找硬编码的 URL。
 */
export const REPO = "https://github.com/yxxbc/Seanbot"
export const RELEASES = `${REPO}/releases`
export const LATEST_DOWNLOAD = `${RELEASES}/latest/download/`
export const ISSUES = `${REPO}/issues`
export const CHANGELOG = `${REPO}/blob/main/CHANGELOG.md`
export const LICENSE = `${REPO}/blob/main/LICENSE`
export const README = `${REPO}#readme`

/** 一行安装命令，与 scripts/install.sh / scripts/install.ps1 保持一致。 */
export const INSTALL_SH =
  "curl -fsSL https://raw.githubusercontent.com/yxxbc/Seanbot/main/scripts/install.sh | sh"
export const INSTALL_PS1 =
  "irm https://raw.githubusercontent.com/yxxbc/Seanbot/main/scripts/install.ps1 | iex"

/**
 * public/ 下的静态资源要带上部署前缀（GitHub Pages 的项目路径 /Seanbot/），
 * 写死路径在本地预览与线上都会 404。
 */
export function assetUrl(path: string): string {
  return `${import.meta.env.BASE_URL}${path.replace(/^\/+/, "")}`
}

