import type { SVGProps } from "react"

/**
 * 品牌图标。
 *
 * 路径来自仓库里原有的内联 sprite（site/assets 时代就有）：它们是品牌标志与
 * 平台标志，颜色固定或用 currentColor 跟随文字，不参与主题切换——所以这里
 * 用固定的品牌色，而不是 text-* / bg-* 这类语义 token。
 */
export function LogoMark(props: SVGProps<SVGSVGElement>) {
  return (
    <svg viewBox="0 0 1024 1024" aria-hidden="true" focusable="false" {...props}>
      <path
        d="M300 620C240 430 360 250 540 270c180 20 260 200 160 340-100 140-310 150-400 10Z"
        fill="none"
        stroke="#E6B85C"
        strokeWidth={86}
        strokeLinecap="round"
      />
      <circle cx={512} cy={512} r={46} fill="#F4E9D8" />
      <circle cx={512} cy={512} r={20} fill="#D95F4B" />
    </svg>
  )
}

function PlatformIcon({ children, ...props }: SVGProps<SVGSVGElement>) {
  return (
    <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true" focusable="false" {...props}>
      {children}
    </svg>
  )
}

export function AppleIcon(props: SVGProps<SVGSVGElement>) {
  return (
    <PlatformIcon {...props}>
      <path d="M16.4 12.6c0-2.2 1.8-3.3 1.9-3.4-1-1.5-2.6-1.7-3.2-1.7-1.4-.1-2.7.8-3.4.8-.7 0-1.8-.8-3-.8-1.5 0-3 .9-3.8 2.3-1.6 2.8-.4 7 1.2 9.3.8 1.1 1.7 2.4 2.9 2.3 1.2 0 1.6-.7 3-.7s1.8.7 3 .7 2-1.1 2.8-2.2c.9-1.3 1.3-2.5 1.3-2.6-.1 0-2.6-1-2.7-3.9Z" />
      <path d="M14.6 5.7c.6-.8 1-1.8.9-2.9-.9 0-2 .6-2.6 1.3-.6.7-1.1 1.8-.9 2.8 1 .1 2-.5 2.6-1.2Z" />
    </PlatformIcon>
  )
}

export function WindowsIcon(props: SVGProps<SVGSVGElement>) {
  return (
    <PlatformIcon {...props}>
      <path d="M3 4.6 10.4 3.5v7.9H3V4.6Zm8.6-1.2L21 2v9.4h-9.4V3.4ZM3 12.6h7.4v7.9L3 19.4v-6.8Zm8.6 0H21V22l-9.4-1.4v-8Z" />
    </PlatformIcon>
  )
}

export function LinuxIcon(props: SVGProps<SVGSVGElement>) {
  return (
    <PlatformIcon {...props}>
      <path d="M12 2.1c-1.9 0-3.3 1.5-3.3 3.5 0 .8.1 1.6-.4 2.5-1.6 2.6-3.1 5-3.1 7.3 0 2.9 3 5.5 6.8 5.5s6.8-2.6 6.8-5.5c0-2.3-1.5-4.7-3.1-7.3-.5-.9-.4-1.7-.4-2.5 0-2-1.4-3.5-3.3-3.5Zm-1.3 3.6c.4 0 .7.4.7.9s-.3.9-.7.9-.7-.4-.7-.9.3-.9.7-.9Zm2.6 0c.4 0 .7.4.7.9s-.3.9-.7.9-.7-.4-.7-.9.3-.9.7-.9Z" />
    </PlatformIcon>
  )
}

export function GithubIcon(props: SVGProps<SVGSVGElement>) {
  return (
    <PlatformIcon {...props}>
      <path d="M12 .5C5.7.5.5 5.7.5 12c0 5 3.3 9.3 7.8 10.8.6.1.8-.3.8-.6v-2c-3.2.7-3.8-1.5-3.8-1.5-.5-1.3-1.3-1.7-1.3-1.7-1-.7.1-.7.1-.7 1.1.1 1.7 1.2 1.7 1.2 1 1.7 2.6 1.2 3.3.9.1-.7.4-1.2.7-1.5-2.6-.3-5.3-1.3-5.3-5.8 0-1.3.5-2.3 1.2-3.1-.1-.3-.5-1.5.1-3.1 0 0 1-.3 3.3 1.2a11.5 11.5 0 0 1 6 0C17.7 4.6 18.7 4.9 18.7 4.9c.6 1.6.2 2.8.1 3.1.8.8 1.2 1.8 1.2 3.1 0 4.5-2.7 5.5-5.3 5.8.4.4.8 1.1.8 2.2v3.3c0 .3.2.7.8.6 4.6-1.5 7.8-5.8 7.8-10.8C23.5 5.7 18.3.5 12 .5Z" />
    </PlatformIcon>
  )
}
