import { useEffect, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'

const iconCache = new Map<string, string>()

type GameIconProps = {
  appId: string
  store: string
  installDir: string
  detectedTitle: string
  matchedTitle?: string | null
  matchedProvider?: string | null
  size?: number
}

export function GameIcon({
  appId,
  store,
  installDir,
  detectedTitle,
  matchedTitle = null,
  matchedProvider = null,
  size = 44,
}: GameIconProps) {
  const cacheKey = `${store}:${appId}:${installDir}:${matchedTitle ?? ''}:${matchedProvider ?? ''}:v2`
  const [iconUrl, setIconUrl] = useState<string | null>(iconCache.get(cacheKey) ?? null)
  const [requested, setRequested] = useState<boolean>(iconCache.has(cacheKey))
  const holderRef = useRef<HTMLDivElement | null>(null)

  useEffect(() => {
    if (requested) {
      return
    }
    const node = holderRef.current
    if (!node) {
      return
    }
    const observer = new IntersectionObserver(
      (entries) => {
        if (!entries[0]?.isIntersecting) {
          return
        }
        observer.disconnect()
        setRequested(true)
        invoke<string>('get_game_icon', {
          appId,
          store,
          installDir,
          detectedTitle,
          matchedTitle,
          matchedProvider,
        })
          .then((url) => {
            iconCache.set(cacheKey, url)
            setIconUrl(url)
          })
          .catch(() => {
            setIconUrl(null)
          })
      },
      { rootMargin: '180px' }
    )
    observer.observe(node)
    return () => observer.disconnect()
  }, [requested, appId, store, installDir, detectedTitle, matchedTitle, matchedProvider, cacheKey])

  return (
    <div
      ref={holderRef}
      className="gameIconWrap"
      style={{ width: `${size}px`, height: `${size}px` }}
      aria-hidden="true"
    >
      {iconUrl ? (
        <img src={iconUrl} alt="" className="gameIconImg" />
      ) : (
        <div className="iconPlaceholder">...</div>
      )}
    </div>
  )
}
