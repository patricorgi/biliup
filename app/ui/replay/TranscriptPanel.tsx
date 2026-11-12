'use client'

import { useEffect, useMemo, useRef, useState } from 'react'
import useSWR from 'swr'
import { Button, Input } from '@douyinfe/semi-ui'
import { IconChevronLeft, IconChevronRight, IconSearch } from '@douyinfe/semi-icons'
import { fetcher } from '@/app/lib/api-streamer'
import { formatPrecise, sessionUrl } from '@/app/lib/sessions'
import styles from './replay.module.scss'

type Cue = { at_ms: number; end_ms?: number; text: string }
type Transcript =
  | { status: 'idle' }
  | { status: 'running'; completed: number; total: number }
  | { status: 'ready'; cues: Cue[] }
  | { status: 'failed'; error: string }

const emptyCues: Cue[] = []

function cueEnd(cues: Cue[], index: number, duration: number) {
  const cue = cues[index]
  if (cue.end_ms !== undefined) return Math.min(cue.end_ms, duration)
  const estimate = Math.max(900, Math.min(4200, [...cue.text].length * 240))
  const next = cues[index + 1]?.at_ms ?? duration
  return Math.min(cue.at_ms + estimate, next > cue.at_ms ? next : duration, duration)
}

function currentCue(cues: Cue[], time: number | null, duration: number) {
  if (time === null) return -1
  let low = 0
  let high = cues.length
  while (low < high) {
    const mid = (low + high) >>> 1
    if (cues[mid].at_ms <= time) low = mid + 1
    else high = mid
  }
  return low > 0 && time < cueEnd(cues, low - 1, duration) ? low - 1 : -1
}

export default function TranscriptPanel({ sessionId, canEdit, recording, onSeek, duration, playhead, playing, saving, onSave }: {
  sessionId: number
  canEdit: boolean
  recording: boolean
  onSeek: (time: number) => void
  duration: number
  playhead: number | null
  playing: boolean
  saving: boolean
  onSave: (start: number, end: number) => Promise<boolean | undefined>
}) {
  const [open, setOpen] = useState(false)
  const [query, setQuery] = useState('')
  const [starting, setStarting] = useState(false)
  const [failure, setFailure] = useState('')
  const [range, setRange] = useState<{ anchor: number; edge: number }>()
  const [following, setFollowing] = useState(true)
  const list = useRef<HTMLUListElement>(null)
  const activeRow = useRef<HTMLLIElement>(null)
  const followedPosition = useRef<number | null>(null)
  const endpoint = sessionUrl(sessionId) + '/transcript'
  const { data, error, mutate } = useSWR<Transcript>(open ? endpoint : null, fetcher, {
    refreshInterval: current => current?.status === 'running' ? 2000 : 0,
  })

  const start = async () => {
    setStarting(true)
    setFailure('')
    try {
      const result: Transcript = await fetcher(endpoint, { method: 'POST' })
      await mutate(result, false)
    } catch (error) {
      setFailure(error instanceof Error ? error.message : '无法开始识别台词')
    } finally {
      setStarting(false)
    }
  }

  const needle = query.trim().toLocaleLowerCase()
  const cues = data?.status === 'ready' ? data.cues : emptyCues
  const matches = useMemo(() => cues.flatMap((cue, index) =>
    cue.text.toLocaleLowerCase().includes(needle) ? [{ cue, index }] : []), [cues, needle])
  const active = currentCue(cues, playhead, duration)
  const first = range ? Math.min(range.anchor, range.edge) : -1
  const last = range ? Math.max(range.anchor, range.edge) : -1
  const startMs = cues[first]?.at_ms
  const endMs = cues[last] ? cueEnd(cues, last, duration) : undefined

  useEffect(() => {
    if (!open || !following || !playing || !list.current || !activeRow.current) return
    const box = list.current.getBoundingClientRect()
    const row = activeRow.current.getBoundingClientRect()
    if (row.top < box.top || row.bottom > box.bottom) {
      list.current.scrollTop += row.top - box.top - box.height / 3
      followedPosition.current = list.current.scrollTop
    }
  }, [active, following, matches, open, playing])

  return open ? (
    <aside className={styles.transcript} aria-label="台词列表">
      <div className={styles.transcriptHeader}>
        <strong>台词列表</strong>
        <Button icon={<IconChevronRight />} aria-label="隐藏台词列表" title="隐藏台词列表" onClick={() => setOpen(false)} />
      </div>
      {error && <p role="alert">台词列表加载失败。 <Button onClick={() => void mutate()}>重试</Button></p>}
      {failure && <p role="alert">{failure}</p>}
      {data?.status === 'failed' && <p role="alert">{data.error}</p>}
      {data?.status === 'running' && (
        <div role="status">
          <p>正在本机识别台词 · 已完成 {data.completed} / {data.total} 段</p>
          <progress aria-label="台词识别进度" value={data.completed} max={data.total} />
        </div>
      )}
      {data && (data.status === 'idle' || data.status === 'failed') && canEdit && (
        <Button theme="solid" disabled={recording || starting} loading={starting} onClick={() => void start()}>
          生成台词列表
        </Button>
      )}
      {data?.status === 'idle' && recording && <p>录制结束后可生成台词。</p>}
      {data?.status === 'idle' && !canEdit && <p>这场录像尚未生成台词。</p>}
      {data?.status === 'ready' && (
        <>
          <Input prefix={<IconSearch />} placeholder="搜索台词" aria-label="搜索台词" value={query} onChange={setQuery} showClear />
          {!following && <Button size="small" onClick={() => setFollowing(true)}>跟随播放</Button>}
          {canEdit && cues.length > 0 && (
            <div className={styles.transcriptSelection}>
              {startMs !== undefined && endMs !== undefined ? (
                <>
                  <span role="status">
                    已选连续 {last - first + 1} 条 · {formatPrecise(startMs)}–{formatPrecise(endMs)}
                    {cues[last].end_ms === undefined ? '（结束时间为估算）' : ''}
                  </span>
                  <div>
                    <Button size="small" theme="solid" disabled={saving || endMs <= startMs}
                      onClick={async () => {
                        if (await onSave(startMs, endMs)) setRange(undefined)
                      }}>存为切片</Button>
                    <Button size="small" disabled={saving} onClick={() => setRange(undefined)}>清除选段</Button>
                  </div>
                </>
              ) : <span>选择一条作为起点，再选到另一条。</span>}
            </div>
          )}
          {matches.length ? (
            <ul className={styles.transcriptList} ref={list}
              onWheel={() => setFollowing(false)}
              onTouchMove={() => setFollowing(false)}
              onScroll={event => {
                const position = event.currentTarget.scrollTop
                if (position !== followedPosition.current) setFollowing(false)
                followedPosition.current = null
              }}>
              {matches.map(({ cue, index }) => (
                <li key={index} ref={index === active ? activeRow : undefined}
                  aria-current={index === active ? 'true' : undefined}
                  data-selected={index >= first && index <= last || undefined}>
                  <button type="button" className={styles.transcriptTime}
                    aria-label={`跳转到 ${formatPrecise(cue.at_ms)}`}
                    onClick={() => onSeek(cue.at_ms)}>{formatPrecise(cue.at_ms)}</button>
                  <span>{cue.text}</span>
                  {canEdit && <Button size="small" theme="borderless" disabled={saving}
                    aria-label={`${range ? '选到' : '选起点'} ${formatPrecise(cue.at_ms)}`}
                    onClick={() => setRange(current => ({ anchor: current?.anchor ?? index, edge: index }))}>
                    {range ? '选到这里' : '选起点'}
                  </Button>}
                </li>
              ))}
            </ul>
          ) : <p>{needle ? '没有找到匹配的台词。' : '没有识别到台词。'}</p>}
        </>
      )}
    </aside>
  ) : (
    <Button className={styles.transcriptToggle} icon={<IconChevronLeft />}
      aria-label="展开台词列表" title="展开台词列表" onClick={() => setOpen(true)} />
  )
}
