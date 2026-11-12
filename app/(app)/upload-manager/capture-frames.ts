import { draftMedia, type DraftSource } from './draft-video'

export const framesPerBatch = 10
const videoEvents = ['loadedmetadata', 'loadeddata', 'canplay', 'durationchange', 'seeked', 'error']
const maxEdge = 1920

function waitForVideo(video: HTMLVideoElement, ready: () => boolean, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const finish = (error?: unknown) => {
      clearTimeout(timer)
      for (const event of videoEvents) video.removeEventListener(event, check)
      signal.removeEventListener('abort', abort)
      if (error) reject(error)
      else resolve()
    }
    const abort = () => finish(signal.reason)
    const check = () => {
      if (video.error) finish(new Error('无法读取此视频，或浏览器不支持该格式。'))
      else if (ready()) finish()
    }
    const timer = setTimeout(() => finish(new Error('读取视频帧超时，请确认视频能正常回看。')), 18000)
    for (const event of videoEvents) video.addEventListener(event, check)
    signal.addEventListener('abort', abort, { once: true })
    if (signal.aborted) abort()
    else check()
  })
}

function jpegFromCanvas(canvas: HTMLCanvasElement): Promise<Blob> {
  return new Promise((resolve, reject) => {
    canvas.toBlob(blob => {
      if (blob) resolve(blob)
      else reject(new Error('无法保存截图。'))
    }, 'image/jpeg', 0.93)
  })
}

export async function captureFrames(
  source: DraftSource,
  batch: number,
  signal: AbortSignal,
  onFrame: (frame: { time: number; blob: Blob }) => void,
) {
  const video = document.createElement('video')
  video.preload = 'auto'
  video.muted = true
  video.playsInline = true
  const localUrl = source.kind === 'local' ? URL.createObjectURL(source.file) : ''
  const { url, type } = draftMedia(source, localUrl)
  const capture = new AbortController()
  const abort = () => capture.abort(signal.reason)
  signal.addEventListener('abort', abort, { once: true })
  let destroyPlayer: (() => void) | undefined

  try {
    signal.throwIfAborted()
    if (type) {
      const [{ default: mpegts }, { destroyMpegts }] = await Promise.all([
        import('mpegts.js'),
        import('@/app/ui/Player'),
      ])
      signal.throwIfAborted()
      if (!mpegts.isSupported()) throw new Error('此浏览器无法解码该视频格式。')
      const player = mpegts.createPlayer({ type, url, isLive: false }, {
        accurateSeek: true,
        lazyLoadMaxDuration: 12,
        lazyLoadRecoverDuration: 3,
      })
      destroyPlayer = () => destroyMpegts(player)
      player.on(mpegts.Events.ERROR, () => capture.abort(new Error('无法读取此视频的画面。')))
      player.attachMediaElement(video)
      player.load()
    } else {
      if (new URL(url, location.href).origin !== location.origin) video.crossOrigin = 'anonymous'
      video.src = url
    }

    await waitForVideo(video, () => Number.isFinite(video.duration) && video.duration > 0 && video.videoWidth > 0, capture.signal)
    const canvas = document.createElement('canvas')
    const context = canvas.getContext('2d')
    if (!context) throw new Error('无法创建封面图片。')
    const scale = Math.min(1, maxEdge / Math.max(video.videoWidth, video.videoHeight))
    canvas.width = Math.max(1, Math.round(video.videoWidth * scale))
    canvas.height = Math.max(1, Math.round(video.videoHeight * scale))
    const duration = video.duration

    // Sample each tenth. Alternate offsets so a refresh never repeats the previous target times.
    for (let index = 0; index < framesPerBatch; index++) {
      capture.signal.throwIfAborted()
      const offset = batch === 0 ? 0.5 : (batch % 2 === 1 ? 0.12 : 0.66) + Math.random() * 0.22
      const position = (index + offset) / framesPerBatch
      video.currentTime = duration * position
      await waitForVideo(video, () => !video.seeking && video.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA, capture.signal)
      context.drawImage(video, 0, 0, canvas.width, canvas.height)
      const blob = await jpegFromCanvas(canvas)
      capture.signal.throwIfAborted()
      onFrame({ time: video.currentTime, blob })
    }
  } finally {
    signal.removeEventListener('abort', abort)
    destroyPlayer?.()
    video.removeAttribute('src')
    video.load()
    if (localUrl) URL.revokeObjectURL(localUrl)
  }
}
