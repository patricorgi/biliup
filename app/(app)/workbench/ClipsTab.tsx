'use client'

import { Button, Checkbox, Input, InputNumber, Modal, TextArea, Toast } from '@douyinfe/semi-ui'
import { useEffect, useRef, useState } from 'react'
import useSWR from 'swr'
import { API_BASE, fetcher, proxy } from '@/app/lib/api-streamer'
import { downloadClip, type Clip } from '@/app/lib/clips'
import { useMe } from '@/app/lib/use-me'
import styles from './clips.module.scss'
import TrashConfirmation from './TrashConfirmation'

const LIBRARY = '/v1/clip-library'
const MAX_SELECTED = 30
const ORDER_STORAGE = 'biliup.workbench.clip-order'
const ORDER_HANDOFF = 'join-order'
type SavedClip = (Clip & { session_title: string }) | {
  id: number
  file_name: string
  source_path: string
  duration_ms: number
}
type Job = {
  id: string
  kind: 'trim' | 'join'
  status: 'queued' | 'running' | 'ready' | 'failed'
  clip_ids: number[]
  created_at: number
  error: string | null
  progress?: { stage: string; ratio: number | null } | null
  cut?: { start_ms: number; end_ms: number } | null
  selection?: { start_ms: number; end_ms: number } | null
  format?: string
  fade_out?: boolean
}
type ImportedClip = Extract<SavedClip, { source_path: string }>
type Subtitle = { status: 'running' | 'ready' } | { status: 'failed'; error: string }
type Hardsub = { status: 'queued' | 'cancelling' | 'cancelled' } | { status: 'running'; ratio: number } | { status: 'ready' } | { status: 'failed'; error: string }
type Library = {
  clips: SavedClip[]
  imports: ImportedClip[]
  jobs: Job[]
  edits: Record<number, string>
  edit_durations_ms: Record<number, number>
  subtitles: Record<string, Subtitle>
  hardsubs: Record<string, Hardsub>
}
type ImportResult = { added: number; duplicates: number; errors: string[] }
type SubtitleDraft = { text: string; revision: string }

const clipName = (clip: SavedClip) => 'source_path' in clip ? clip.file_name : clip.title.trim() || `未命名切片 #${clip.id}`
const outputUrl = (id: string) => `${API_BASE}${LIBRARY}/output/${encodeURIComponent(id)}`
const subtitleRoute = (id: string) => `${LIBRARY}/output/${encodeURIComponent(id)}/subtitles`
const hardsubRoute = (id: string) => `${LIBRARY}/output/${encodeURIComponent(id)}/hardsubs`
const jobStatus = { queued: '排队中', running: '处理中', ready: '已保存', failed: '失败' }
const isProcessing = ({ status }: { status: Job['status'] }) => status === 'queued' || status === 'running'
const seconds = (ms: number) => Number((ms / 1000).toFixed(3))
const errorText = (error: unknown) => error instanceof Error ? error.message : '操作失败，请重试'

function readOrder(): number[] {
  if (typeof window === 'undefined') return []
  const handoff = new URLSearchParams(window.location.search).get(ORDER_HANDOFF)
  try {
    const values: unknown = handoff === null
      ? JSON.parse(window.sessionStorage.getItem(ORDER_STORAGE) ?? '[]')
      : handoff.split(',').filter(Boolean).map(Number)
    if (!Array.isArray(values)) return []
    return [...new Set(values.filter((id): id is number => Number.isSafeInteger(id) && id !== 0))].slice(0, MAX_SELECTED)
  } catch {
    return []
  }
}

function formatDuration(ms: number) {
  const tenths = Math.round(ms / 100)
  const wholeSeconds = Math.floor(tenths / 10)
  const hours = Math.floor(wholeSeconds / 3600)
  const minutes = Math.floor(wholeSeconds / 60) % 60
  const seconds = String(wholeSeconds % 60).padStart(2, '0')
  const prefix = hours > 0 ? `${hours}:${String(minutes).padStart(2, '0')}` : String(minutes)
  return `${prefix}:${seconds}${tenths % 10 ? `.${tenths % 10}` : ''}`
}

function initialRange(clip: SavedClip, edited: boolean) {
  if (edited) return { start: 0, end: 0, duration: 0 }
  if ('source_path' in clip) {
    const duration = seconds(clip.duration_ms)
    return { start: 0, end: duration, duration }
  }
  const duration = clip.duration_ms ?? Math.max(0, (clip.cut_out_ms ?? clip.out_ms) - (clip.cut_in_ms ?? clip.in_ms))
  const start = Math.max(0, clip.in_ms - (clip.cut_in_ms ?? clip.in_ms))
  const end = Math.min(duration, clip.out_ms - (clip.cut_in_ms ?? clip.in_ms))
  return {
    start: seconds(start < end ? start : 0),
    end: seconds(start < end ? end : duration),
    duration: seconds(duration),
  }
}

export default function ClipsTab({ active }: { active: boolean }) {
  const { data, error, isLoading, mutate } = useSWR<Library>(active ? LIBRARY : null, fetcher, {
    refreshInterval: current => current && (
      current.jobs.some(isProcessing) || Object.values(current.subtitles).some(item => item.status === 'running')
      || Object.values(current.hardsubs).some(item => item.status === 'queued' || item.status === 'running' || item.status === 'cancelling')
    ) ? 1200 : 5000,
  })
  const { can } = useMe()
  const editable = can('clip.edit')
  const canImport = can('file.import')
  const canDelete = can('file.delete')
  const [query, setQuery] = useState('')
  const [importOpen, setImportOpen] = useState(false)
  const [order, setOrder] = useState(readOrder)
  useEffect(() => {
    try {
      window.sessionStorage.setItem(ORDER_STORAGE, JSON.stringify(order))
      const url = new URL(window.location.href)
      if (url.searchParams.has(ORDER_HANDOFF)) {
        url.searchParams.delete(ORDER_HANDOFF)
        window.history.replaceState(null, '', url)
      }
    } catch {
      // The selection is still available in this page when storage is disabled.
    }
  }, [order])
  const [editor, setEditor] = useState<SavedClip>()
  const [preview, setPreview] = useState<Job>()
  const [subtitleEditor, setSubtitleEditor] = useState<Job>()
  const [joinToTrash, setJoinToTrash] = useState<Job>()
  const [trashing, setTrashing] = useState(false)
  const [clipsToTrash, setClipsToTrash] = useState<SavedClip[]>()
  const [trashingClips, setTrashingClips] = useState(false)
  const [trashClipError, setTrashClipError] = useState('')
  const [joining, setJoining] = useState(false)
  const [fadeOut, setFadeOut] = useState(false)
  const [downloading, setDownloading] = useState<number>()
  const [restoring, setRestoring] = useState<number>()

  const clips = [...(data?.imports ?? []), ...(data?.clips ?? [])]
  const jobs = data?.jobs ?? []
  const results = jobs.filter(job => !job.selection)
  const byId = new Map(clips.map(clip => [clip.id, clip]))
  const edits = new Map<number, Job>()
  const editing = new Set<number>()
  for (const job of jobs) {
    if (job.kind !== 'trim') continue
    const id = job.clip_ids[0]
    if (isProcessing(job)) editing.add(id)
    if (job.status === 'ready' && data?.edits?.[id] === job.id) edits.set(id, job)
  }
  const selected = order.filter(id => byId.has(id))
  const totalDuration = selected.reduce<number | null>((total, id) => {
    const clip = byId.get(id)!
    const duration = edits.has(id) ? data?.edit_durations_ms?.[id] : clip.duration_ms
    return total !== null && duration != null ? total + duration : null
  }, 0)
  const visible = clips.filter(clip =>
    [clipName(clip), 'source_path' in clip ? clip.source_path : clip.session_title, clip.file_name].some(text => text?.toLowerCase().includes(query.trim().toLowerCase())))

  const toggle = (id: number) => {
    setOrder(selected.includes(id) ? selected.filter(item => item !== id) : [...selected, id])
  }

  const move = (index: number, step: number) => {
    const next = [...selected]
    const target = index + step
    if (target < 0 || target >= next.length) return
    const [clip] = next.splice(index, 1)
    next.splice(target, 0, clip)
    setOrder(next)
  }

  const submit = async (route: string, body: object) => {
    const job: Job = await fetcher(LIBRARY + route, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    })
    void mutate(current => current && {
      ...current,
      jobs: [job, ...current.jobs.filter(item => item.id !== job.id)],
      ...(job.selection && {
        edits: { ...current.edits, [job.clip_ids[0]]: job.id },
        edit_durations_ms: { ...current.edit_durations_ms, [job.clip_ids[0]]: job.selection.end_ms - job.selection.start_ms },
      }),
    }, { revalidate: false })
    Toast.success(job.selection ? '已保存有效范围' : '已加入处理队列')
  }

  const join = async () => {
    if (joining || selected.length < 2) return
    setJoining(true)
    try {
      await submit('/join', {
        items: selected.map(clip_id => ({ clip_id })),
        source_format: !fadeOut,
        fade_out: fadeOut,
      })
    } catch (err) {
      Toast.error(errorText(err))
    } finally {
      setJoining(false)
    }
  }

  const restore = async (id: number) => {
    setRestoring(id)
    try {
      await proxy(`${LIBRARY}/restore/${id}`, { method: 'POST' })
      await mutate()
      Toast.success('已还原原切片')
    } catch (error) {
      Toast.error(errorText(error))
    } finally {
      setRestoring(undefined)
    }
  }

  const trashJoin = async () => {
    if (!joinToTrash || trashing) return
    setTrashing(true)
    try {
      await proxy(`${LIBRARY}/output/${encodeURIComponent(joinToTrash.id)}`, { method: 'DELETE' })
      void mutate(current => current && {
        ...current,
        jobs: current.jobs.filter(job => job.id !== joinToTrash.id),
      })
      setJoinToTrash(undefined)
      Toast.success('已移入废纸篓')
    } catch (error) {
      Toast.error(errorText(error))
    } finally {
      setTrashing(false)
    }
  }

  const trashClips = async () => {
    if (!clipsToTrash || trashingClips) return
    setTrashingClips(true)
    setTrashClipError('')
    try {
      const result: { removed: number[]; errors: string[] } = await fetcher(LIBRARY, {
        method: 'DELETE',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ ids: clipsToTrash.map(clip => clip.id) }),
      })
      const removed = new Set(result.removed)
      if (removed.size) {
        setOrder(current => current.filter(id => !removed.has(id)))
        Toast.success(`已将 ${removed.size} 个切片及其精剪移入废纸篓`)
      }
      void mutate()
      if (result.errors.length) {
        setClipsToTrash(clipsToTrash.filter(clip => !removed.has(clip.id)))
        setTrashClipError(result.errors.join('；'))
      } else {
        setClipsToTrash(undefined)
      }
    } catch (error) {
      setTrashClipError(errorText(error))
    } finally {
      setTrashingClips(false)
    }
  }

  const download = async (clip: SavedClip) => {
    setDownloading(clip.id)
    try {
      await downloadClip(clip, 'source')
    } catch (err) {
      Toast.error(errorText(err))
    } finally {
      setDownloading(undefined)
    }
  }

  return (
    <div className={styles.layout}>
      <div className={editable ? `${styles.selection} ${styles.withQueue}` : styles.selection}>
        <section className={styles.card} aria-label="已保存的切片">
          <div className={styles.heading}>
            <h2>已保存的切片</h2>
            <div className={styles.buttons}>
              <Input aria-label="搜索切片" placeholder="搜索切片或场次" value={query} onChange={setQuery} showClear />
              {canImport && <Button onClick={() => setImportOpen(true)}>导入外部切片</Button>}
              {canDelete && <Button type="danger" disabled={!selected.length || joining}
                onClick={() => {
                  setTrashClipError('')
                  setClipsToTrash(selected.map(id => byId.get(id)!))
                }}>删除选中{selected.length ? `（${selected.length}）` : ''}</Button>}
            </div>
          </div>
          <p className={styles.note}>快速剪的原片已保存在录制电脑的 <code>clips</code> 目录，剪好后可以直接在这里使用。</p>
          {isLoading ? <p>正在加载切片…</p> : error ? (
            <p role="alert">切片列表加载失败。<Button onClick={() => void mutate()}>重试</Button></p>
          ) : !visible.length ? (
            <p className={styles.empty}>{query ? '没有匹配的切片' : '在回看页面完成快速剪后，切片会出现在这里。'}</p>
          ) : (
            <ul className={styles.list}>
              {visible.map(clip => {
                const ext = clip.file_name?.split('.').pop()?.toUpperCase()
                const edit = edits.get(clip.id)
                const subtitle = 'source_path' in clip
                  ? clip.source_path
                    : [clip.session_title, edit && !edit.selection ? 'MP4' : ext, !edit && clip.duration_ms && `${seconds(clip.duration_ms)} 秒`].filter(Boolean).join(' · ')
                return (
                  <li className={styles.row} key={clip.id}>
                    {(editable || canDelete) && <Checkbox
                      aria-label={`选择 ${clipName(clip)}`}
                      checked={selected.includes(clip.id)}
                      disabled={!selected.includes(clip.id) && selected.length >= MAX_SELECTED}
                      onChange={() => toggle(clip.id)}
                    />}
                    <div className={styles.rowText}>
                      <strong>{clipName(clip)}</strong>
                      <span title={subtitle}>{subtitle}</span>
                      {edit?.selection
                        ? <span>有效范围 {seconds(edit.selection.start_ms)}–{seconds(edit.selection.end_ms)} 秒</span>
                        : edit && <span>已精剪</span>}
                    </div>
                    <div className={styles.buttons}>
                      {editable && <Button size="small" loading={editing.has(clip.id)} onClick={() => setEditor(clip)}>精剪</Button>}
                      {editable && edit && <Button size="small" loading={restoring === clip.id}
                        disabled={editing.has(clip.id) || restoring !== undefined} onClick={() => restore(clip.id)}>还原原切片</Button>}
                      {edit && !edit.selection
                        ? <a href={outputUrl(edit.id) + '?download=true'} download>下载切片</a>
                        : 'source_path' in clip
                          ? <a href={`${API_BASE}${LIBRARY}/imported/${clip.id}/download`} download>下载原切片</a>
                          : <Button size="small" loading={downloading === clip.id} onClick={() => download(clip)}>下载原切片</Button>}
                    </div>
                  </li>
                )
              })}
            </ul>
          )}
        </section>
        {editable && <section className={`${styles.card} ${styles.queue}`} aria-label="拼接顺序">
          <div className={styles.heading}>
            <h2>拼接顺序</h2>
            <Button theme="solid" type="primary" disabled={selected.length < 2} loading={joining} onClick={join}>拼接</Button>
          </div>
          <p className={styles.total} aria-live="polite">
            <span>拼接后总时长</span>
            <strong>{totalDuration === null ? '暂无法估算' : `${selected.length ? '约 ' : ''}${formatDuration(totalDuration)}`}</strong>
          </p>
          <Checkbox checked={fadeOut} disabled={joining} onChange={event => setFadeOut(Boolean(event.target.checked))}>
            结尾淡出
          </Checkbox>
          {fadeOut && <p className={styles.note}>片尾画面淡到黑色，重新编码为 MP4。</p>}
          {!selected.length ? <p className={styles.empty}>勾选两个或更多切片，再调整播放顺序。</p> : (
            <ol className={styles.sequence}>
              {selected.map((id, index) => {
                const clip = byId.get(id)!
                return (
                  <li key={id}>
                    <div className={styles.sequenceRow}>
                      <strong>{clipName(clip)}</strong>
                      <div className={styles.buttons}>
                        <Button size="small" aria-label={`上移 ${clipName(clip)}`} disabled={index === 0} onClick={() => move(index, -1)}>↑</Button>
                        <Button size="small" aria-label={`下移 ${clipName(clip)}`} disabled={index === selected.length - 1} onClick={() => move(index, 1)}>↓</Button>
                        <Button size="small" theme="borderless" onClick={() => toggle(id)}>移除</Button>
                      </div>
                    </div>
                    {edits.has(id) && <span className={styles.note}>已精剪</span>}
                  </li>
                )
              })}
            </ol>
          )}
        </section>}
      </div>
      <section className={styles.card} aria-label="处理结果">
        <h2>处理结果</h2>
        <p className={styles.note}>成片和字幕 SRT 保存在 <code>clips/managed</code>。拼接成片可以在本机生成简体中文字幕。</p>
        {!results.length ? <p className={styles.empty}>暂无处理结果</p> : (
          <ul className={styles.list}>
            {results.map(job => {
              const subtitle = data?.subtitles[job.id]
              const hardsub = data?.hardsubs[job.id]
              const format = (job.format ?? 'mp4').toUpperCase()
              const deletable = canDelete && job.kind === 'join' && !isProcessing(job)
              return (
                <li key={job.id} className={styles.row}>
                  <div className={styles.rowText}>
                    {job.kind === 'join' ? (
                      <details className={styles.joinDetails}>
                        <summary><strong>拼接 · {job.clip_ids.length} 个切片{job.fade_out ? ' · 结尾淡出' : ''}</strong></summary>
                        <ol aria-label="拼接切片">
                          {job.clip_ids.map(id => <li key={id}>{byId.has(id) ? clipName(byId.get(id)!) : `切片 #${id}`}</li>)}
                        </ol>
                      </details>
                    ) : <strong>精剪 · {byId.has(job.clip_ids[0]) ? clipName(byId.get(job.clip_ids[0])!) : `切片 #${job.clip_ids[0]}`}</strong>}
                    <span>{jobStatus[job.status]} · {new Date(job.created_at).toLocaleString()}</span>
                    {job.cut && <span>实际裁切 {seconds(job.cut.start_ms)}–{seconds(job.cut.end_ms)} 秒</span>}
                    {isProcessing(job) && <JobProgress job={job} />}
                    {job.error && <span role="alert">{job.error}</span>}
                    {subtitle?.status === 'running' && (
                      <div className={styles.progress} role="status">
                        <progress aria-label="正在生成字幕" />
                        <span>正在生成字幕</span>
                      </div>
                    )}
                    {subtitle?.status === 'failed' && <span role="alert">{subtitle.error}</span>}
                    {hardsub?.status === 'queued' && <span role="status">内嵌字幕排队中</span>}
                    {hardsub?.status === 'running' && (
                      <div className={styles.progress} role="status">
                        <progress aria-label="正在生成内嵌字幕" max={100} value={Math.floor(hardsub.ratio * 100)} />
                        <span>正在生成内嵌字幕 · {Math.floor(hardsub.ratio * 100)}%</span>
                      </div>
                    )}
                    {hardsub?.status === 'failed' && <span role="alert">{hardsub.error}</span>}
                    {hardsub?.status === 'cancelling' && <span role="status">正在取消内嵌字幕</span>}
                    {hardsub?.status === 'cancelled' && <span role="status">已取消内嵌字幕</span>}
                  </div>
                  {(job.status === 'ready' || deletable) && <div className={styles.buttons}>
                    {job.status === 'ready' && <>
                      <Button size="small" onClick={() => setPreview(job)}>播放</Button>
                      <a href={outputUrl(job.id) + '?download=true'} download>下载 {format}</a>
                      {job.kind === 'join' && format !== 'MP4' && <a href={outputUrl(job.id) + '?download=true&mp4=true'} download>导出 MP4</a>}
                      {job.kind === 'join' && <ExportAction
                        route={subtitleRoute(job.id)}
                        state={subtitle}
                        editable={editable}
                        generateLabel="生成字幕"
                        downloadLabel="下载 SRT"
                        onUpdate={state => void mutate(current => current && {
                          ...current,
                          subtitles: { ...current.subtitles, [job.id]: state },
                        }, { revalidate: false })}
                      />}
                      {editable && subtitle?.status === 'ready' && <Button size="small"
                        onClick={() => setSubtitleEditor(job)}>编辑字幕</Button>}
                      {job.kind === 'join' && (subtitle?.status === 'ready' || hardsub?.status === 'ready') && <ExportAction
                        route={hardsubRoute(job.id)}
                        state={hardsub}
                        editable={editable}
                        generateLabel="生成内嵌字幕"
                        downloadLabel="下载带字幕 MP4"
                        cancellable
                        onUpdate={state => void mutate(current => current && {
                          ...current,
                          hardsubs: { ...current.hardsubs, [job.id]: state },
                        }, { revalidate: false })}
                      />}
                    </>}
                    {deletable && <Button type="danger" size="small" theme="borderless"
                      onClick={() => setJoinToTrash(job)}>删除</Button>}
                  </div>}
                </li>
              )
            })}
          </ul>
        )}
      </section>
      {importOpen && <ImportDialog
        onClose={() => setImportOpen(false)}
        onImported={() => void mutate()}
      />}
      {editor && <TrimDialog
        key={editor.id}
        clip={editor}
        edit={edits.get(editor.id)}
        onClose={() => setEditor(undefined)}
        onSave={(start_ms, end_ms) => submit(`/trim/${editor.id}`, { start_ms, end_ms })}
      />}
      {preview && <Modal visible title="播放处理结果" footer={null} onCancel={() => setPreview(undefined)}>
        <video className={styles.video} src={outputUrl(preview.id)} controls autoPlay playsInline>
          {data?.subtitles[preview.id]?.status === 'ready' && (
            <track
              kind="subtitles"
              label="简体中文"
              srcLang="zh-Hans"
              src={API_BASE + subtitleRoute(preview.id) + '?vtt=true'}
              default
            />
          )}
        </video>
      </Modal>}
      {subtitleEditor && <SubtitleEditor key={subtitleEditor.id} id={subtitleEditor.id}
        onClose={() => setSubtitleEditor(undefined)} onSaved={() => void mutate()} />}
      {clipsToTrash && <TrashConfirmation visible title="删除选中的切片？" pending={trashingClips}
        onConfirm={() => void trashClips()} onCancel={() => setClipsToTrash(undefined)}>
        <p>将 {clipsToTrash.length} 个原切片和所有关联的精剪文件移入废纸篓，并清除已设定的精剪范围。</p>
        {clipsToTrash.some(clip => 'source_path' in clip) && <p>外部导入的原文件也会从原位置移入废纸篓。</p>}
        <ul>{clipsToTrash.map(clip => <li key={clip.id}>{clipName(clip)}</li>)}</ul>
        <p>已完成的拼接成片和完整直播录像会保留。</p>
        {trashClipError && <p role="alert">{trashClipError}</p>}
      </TrashConfirmation>}
      {joinToTrash && <TrashConfirmation visible title="删除拼接结果？" pending={trashing}
        onConfirm={() => void trashJoin()} onCancel={() => setJoinToTrash(undefined)}>
        <p>将 {joinToTrash.clip_ids.length} 个切片拼成的结果及其字幕文件移入废纸篓。</p>
      </TrashConfirmation>}
    </div>
  )
}

function SubtitleEditor({ id, onClose, onSaved }: { id: string; onClose: () => void; onSaved: () => void }) {
  const route = subtitleRoute(id) + '/edit'
  const { data, error, mutate } = useSWR<SubtitleDraft>(route, fetcher, { revalidateOnFocus: false })
  const [edited, setEdited] = useState<string>()
  const [saving, setSaving] = useState(false)
  const [failure, setFailure] = useState('')
  const text = edited ?? data?.text ?? ''

  const save = async () => {
    if (!data || saving || !text.trim()) return
    setSaving(true)
    setFailure('')
    try {
      await fetcher(route, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ text, revision: data.revision }),
      })
      void mutate()
      onSaved()
      onClose()
      Toast.success('字幕已保存并同步')
    } catch (error) {
      setFailure(errorText(error))
    } finally {
      setSaving(false)
    }
  }

  return (
    <Modal visible title="编辑字幕" width={680} okText="保存并同步时间线" cancelText="取消"
      maskClosable={false} closable={!saving} closeOnEsc={!saving} confirmLoading={saving}
      okButtonProps={{ disabled: !data || !text.trim() }}
      onOk={save} onCancel={() => { if (!saving) onClose() }}>
      <p className={styles.note}>每行单独显示。按回车断句，超过 16 个字符的行会自动分段。</p>
      <TextArea aria-label="字幕文字" value={text} onChange={setEdited} rows={12} disabled={!data || saving} />
      {saving && <p role="status">正在同步字幕时间线…</p>}
      {(error || failure) && <p role="alert">{failure || errorText(error)}</p>}
    </Modal>
  )
}

function ExportAction<T extends Subtitle | Hardsub>({ route, state, editable, generateLabel, downloadLabel, cancellable, onUpdate }: {
  route: string
  state?: T
  editable: boolean
  generateLabel: string
  downloadLabel: string
  cancellable?: boolean
  onUpdate: (state: T) => void
}) {
  const [submitting, setSubmitting] = useState(false)
  if (state?.status === 'ready') return <a href={API_BASE + route} download>{downloadLabel}</a>
  if (!editable) return null
  const running = state?.status === 'running' || state?.status === 'queued'
  const canCancel = cancellable && running

  const request = async (method: 'POST' | 'DELETE') => {
    setSubmitting(true)
    try {
      onUpdate(await fetcher(route, { method }))
    } catch (error) {
      Toast.error(errorText(error))
    } finally {
      setSubmitting(false)
    }
  }

  const click = () => {
    if (canCancel) {
      Modal.confirm({
        title: '取消生成内嵌字幕？',
        okText: '取消生成',
        cancelText: '继续生成',
        onOk: () => request('DELETE'),
      })
    } else {
      void request('POST')
    }
  }
  return <Button size="small" loading={submitting || state?.status === 'cancelling' || (running && !canCancel)}
    title={canCancel ? '再次点击可取消' : undefined} onClick={click}>
    {state?.status === 'failed' ? `重试${generateLabel}` : generateLabel}
  </Button>
}

function JobProgress({ job }: { job: Job }) {
  const ratio = job.progress?.ratio
  const percent = ratio == null ? undefined : Math.floor(ratio * 100)
  const stage = job.progress?.stage ?? (job.status === 'queued' ? '等待处理' : '准备中')
  return (
    <div className={styles.progress}>
      <progress aria-label={stage} max={100} value={percent} />
      <span>{stage}{percent !== undefined && ` · ${percent}%`}</span>
    </div>
  )
}

function ImportDialog({ onClose, onImported }: { onClose: () => void; onImported: () => void }) {
  const [paths, setPaths] = useState('')
  const [pending, setPending] = useState(false)
  const [result, setResult] = useState<ImportResult>()
  const submitted = paths.split(/\r?\n/).map(path => path.trim()).filter(Boolean)
  const submit = async () => {
    if (pending || !submitted.length) return
    setPending(true)
    setResult(undefined)
    try {
      const summary: ImportResult = await fetcher(LIBRARY + '/import', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ paths: submitted }),
      })
      onImported()
      if (summary.errors.length) {
        setResult(summary)
      } else {
        Toast.success(`已导入 ${summary.added} 个切片${summary.duplicates ? `，跳过 ${summary.duplicates} 个重复文件` : ''}`)
        onClose()
      }
    } catch (error) {
      Toast.error(errorText(error))
    } finally {
      setPending(false)
    }
  }

  return (
    <Modal visible title="导入外部切片" okText="导入" cancelText="取消" maskClosable={false}
      confirmLoading={pending} okButtonProps={{ disabled: !submitted.length }}
      closable={!pending} closeOnEsc={!pending}
      onOk={submit} onCancel={() => { if (!pending) onClose() }}>
      <p>每行输入一个文件或文件夹路径。导入后可直接使用原位置的文件。</p>
      <TextArea aria-label="文件或文件夹路径" rows={5} value={paths} onChange={setPaths}
        disabled={pending} placeholder="~/Downloads/直播切片" />
      {result && <div role="status">
        <p>已导入 {result.added} 个，跳过 {result.duplicates} 个重复文件。</p>
        <ul className={styles.importErrors}>{result.errors.map((error, i) => <li key={i}>{error}</li>)}</ul>
      </div>}
    </Modal>
  )
}

function TrimDialog({ clip, edit, onClose, onSave }: {
  clip: SavedClip
  edit?: Job
  onClose: () => void
  onSave: (start: number, end: number) => Promise<void>
}) {
  const selected = edit?.selection
  const lower = selected ? seconds(selected.start_ms) : 0
  const upper = selected ? seconds(selected.end_ms) : undefined
  const initial = selected
    ? { start: 0, end: seconds(selected.end_ms - selected.start_ms), duration: seconds(selected.end_ms - selected.start_ms) }
    : initialRange(clip, Boolean(edit))
  const [duration, setDuration] = useState(initial.duration)
  const [start, setStart] = useState<number | string>(initial.start)
  const [end, setEnd] = useState<number | string>(initial.end)
  const [saving, setSaving] = useState(false)
  const [previewFailed, setPreviewFailed] = useState(false)
  const [previewAttempt, setPreviewAttempt] = useState(0)
  const video = useRef<HTMLVideoElement>(null)
  const valid = start !== '' && end !== '' && Number(start) >= 0 && Number(end) > Number(start)
    && (duration === 0 || Number(end) <= duration + 0.001)

  const save = async () => {
    if (!valid || saving) return
    setSaving(true)
    try {
      await onSave(Math.round(Number(start) * 1000), Math.round(Number(end) * 1000))
      onClose()
    } catch (error) {
      Toast.error(errorText(error))
      setSaving(false)
    }
  }

  const currentTime = () => seconds(Math.max(0, (video.current?.currentTime ?? lower) - lower) * 1000)
  const previewUrl = `${API_BASE}${LIBRARY}/preview/${clip.id}`

  return (
    <Modal visible title={`精剪 · ${clipName(clip)}`} okText="保存精剪" cancelText="取消"
      onOk={save} onCancel={() => { if (!saving) onClose() }}
      confirmLoading={saving} okButtonProps={{ disabled: !valid }} maskClosable={false}>
      <video
        key={previewAttempt}
        ref={video}
        className={styles.video}
        src={selected ? `${previewUrl}?range=${encodeURIComponent(edit!.id)}#t=${lower},${upper}`
          : edit ? outputUrl(edit.id) : previewUrl}
        controls playsInline preload="metadata"
        onError={() => setPreviewFailed(true)}
        onLoadedMetadata={event => {
          const player = event.currentTarget
          if (upper !== undefined) player.currentTime = lower
          const actual = upper !== undefined ? upper - lower : player.duration
          if (Number.isFinite(actual) && actual > 0) {
            setDuration(seconds(actual * 1000))
            setEnd(current => Number(current) === 0 || Number(current) > actual ? seconds(actual * 1000) : current)
          }
        }}
        onPlay={event => {
          if (upper !== undefined && event.currentTarget.currentTime >= upper - 0.001) {
            event.currentTarget.currentTime = lower
          }
        }}
        onSeeking={event => {
          const player = event.currentTarget
          if (upper !== undefined) {
            if (player.currentTime < lower) player.currentTime = lower
            else if (player.currentTime > upper) player.currentTime = upper
          }
        }}
        onTimeUpdate={event => {
          const player = event.currentTarget
          if (upper !== undefined && player.currentTime >= upper) {
            player.pause()
            if (player.currentTime > upper) player.currentTime = upper
          }
        }}
      />
      {previewFailed && <p role="alert">
        预览暂时无法播放。
        <Button size="small" onClick={() => {
          setPreviewFailed(false)
          setPreviewAttempt(current => current + 1)
        }}>重试预览</Button>
      </p>}
      <p className={styles.note}>只保存有效范围。拼接时直接复制音视频，入点会对齐关键帧。</p>
      <div className={styles.range}>
        <label>入点 <InputNumber aria-label="入点（秒）" value={start} onChange={setStart} min={0} step={0.001} /></label>
        <Button onClick={() => setStart(currentTime())} disabled={previewFailed}>取播放位置为入点</Button>
        <label>出点 <InputNumber aria-label="出点（秒）" value={end} onChange={setEnd} min={0} step={0.001} /></label>
        <Button onClick={() => setEnd(currentTime())} disabled={previewFailed}>取播放位置为出点</Button>
      </div>
    </Modal>
  )
}
