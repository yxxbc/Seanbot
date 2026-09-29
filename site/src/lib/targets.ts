import { INSTALL_PS1, INSTALL_SH } from "@/lib/site"

/**
 * 平台 / 架构 → 发布产物。
 *
 * target 必须与 .github/workflows/release.yml 的 matrix 以及 scripts/package.sh
 * 的命名一致，否则页面上的下载链接会 404；扩展名同样写死在这里。
 */
export type OsId = "macos" | "windows" | "linux"
export type ArchId = "arm64" | "x64"

export interface Arch {
  id: ArchId
  /** 按钮上的名字 */
  label: string
  /** CI 真实构建的三元组 */
  target: string
  /** 归档扩展名 */
  ext: string
}

export interface Platform {
  id: OsId
  label: string
  /** 自动识别到你之后显示的名字 */
  detectNote: string
  defaultArch: ArchId
  archs: Arch[]
  install: string
  installNote: string
}

export const TARGETS: Record<OsId, Platform> = {
  macos: {
    id: "macos",
    label: "macOS",
    detectNote: "macOS",
    defaultArch: "arm64",
    archs: [
      { id: "arm64", label: "Apple 芯片", target: "aarch64-apple-darwin", ext: "tar.gz" },
      { id: "x64", label: "Intel", target: "x86_64-apple-darwin", ext: "tar.gz" },
    ],
    install: INSTALL_SH,
    installNote:
      "装到 ~/.local/bin；之后用 `sean update` 升级，不必回官网。可用 SEANBOT_VERSION、SEANBOT_INSTALL_DIR 覆盖默认行为。",
  },
  windows: {
    id: "windows",
    label: "Windows",
    detectNote: "Windows",
    defaultArch: "x64",
    archs: [{ id: "x64", label: "x64", target: "x86_64-pc-windows-msvc", ext: "zip" }],
    install: INSTALL_PS1,
    installNote:
      "在 PowerShell 5.1+ 里运行；升级重跑这条命令即可（Windows 不能在运行时替换自身）。ARM 设备用 x64 包（系统自带模拟）。",
  },
  linux: {
    id: "linux",
    label: "Linux",
    detectNote: "Linux",
    defaultArch: "x64",
    archs: [
      { id: "x64", label: "x86_64", target: "x86_64-unknown-linux-gnu", ext: "tar.gz" },
      { id: "arm64", label: "ARM64", target: "aarch64-unknown-linux-gnu", ext: "tar.gz" },
    ],
    install: INSTALL_SH,
    installNote:
      "需要 curl 与 tar；构建基于 glibc。安装脚本会自动挑对应架构的包，升级重跑这条命令或 `sean update`。",
  },
}

export const OS_IDS: OsId[] = ["macos", "windows", "linux"]

export function platformOf(os: OsId): Platform {
  return TARGETS[os] ?? TARGETS.macos
}

export function archOf(os: OsId, id: ArchId): Arch {
  const archs = platformOf(os).archs
  return archs.find((arch) => arch.id === id) ?? archs[0]
}

/** 只有多于一种架构时才需要让用户选（Windows 只有 x64）。 */
export function needsArchChoice(os: OsId): boolean {
  return platformOf(os).archs.length > 1
}

/** 发布产物文件名，例如 sean-aarch64-apple-darwin.tar.gz。 */
export function artifactName(os: OsId, id: ArchId): string {
  const arch = archOf(os, id)
  return `sean-${arch.target}.${arch.ext}`
}

/**
 * 浏览器环境的系统 / 架构识别。
 *
 * 传进来的是 window.navigator 的“最小接口”，方便在没有浏览器的环境里推理；
 * 所有探测都可能失败，失败时返回 null，由调用方退回默认值。
 */
export interface NavigatorLike {
  userAgent?: string
  userAgentData?: {
    platform?: string
    getHighEntropyValues?: (
      hints: string[],
    ) => Promise<{ architecture?: string; bitness?: string }>
  }
}

/** 手机 / 平板跑不了 Seanbot；iPad 的 UA 里带着 "like Mac OS X"，必须先拦移动端。 */
export function isMobile(nav: NavigatorLike): boolean {
  return /Android|iPhone|iPad|iPod|Mobile/i.test(nav.userAgent ?? "")
}

export function detectOS(nav: NavigatorLike): OsId | null {
  if (isMobile(nav)) return null

  const platform = (nav.userAgentData?.platform ?? "").toLowerCase()
  const ua = nav.userAgent ?? ""

  if (platform.startsWith("mac")) return "macos"
  if (platform.includes("windows")) return "windows"
  if (platform.includes("linux") || platform.includes("chrome os")) return "linux"

  if (/Windows NT/i.test(ua)) return "windows"
  if (/Macintosh|Mac OS X/i.test(ua)) return "macos"
  if (/Linux|X11|CrOS/i.test(ua)) return "linux"

  return null
}

/** Chromium 系浏览器可以问出 CPU 架构；Safari / Firefox 拿不到，就用默认值。 */
export async function detectArch(nav: NavigatorLike): Promise<ArchId | null> {
  const uaData = nav.userAgentData
  if (!uaData?.getHighEntropyValues) return null

  try {
    const values = await uaData.getHighEntropyValues(["architecture", "bitness"])
    const arch = String(values.architecture ?? "").toLowerCase()
    if (arch === "arm") return "arm64"
    if (arch === "x86" && String(values.bitness) === "64") return "x64"
    return null
  } catch {
    return null
  }
}
