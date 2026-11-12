'use client'
import {
  Button,
  ButtonGroup,
  List,
  Popconfirm,
  Notification,
  Typography,
  Modal,
  Transfer,
  Card,
  Tag,
  Tooltip,
  Banner,
  Form,
} from '@douyinfe/semi-ui'
import {
  IconCloudStroked,
  IconPlusCircle,
  IconUserListStroked,
  IconEdit2Stroked,
  IconSendStroked,
  IconDeleteStroked,
} from '@douyinfe/semi-icons'
import { useEffect, useRef, useState } from 'react'
import dynamic from 'next/dynamic'
import Link from 'next/link'
import { fetcher, FileList, proxy, requestDelete, StudioEntity } from '../../lib/api-streamer'
import useSWR from 'swr'
import { useRouter } from 'next/navigation'
import UserList from '../../ui/UserList'
import TemplateFields from '../../ui/TemplateFields'
import type { FormApi } from '@douyinfe/semi-ui/lib/es/form'
import useSWRMutation from 'swr/mutation'
import { useBiliUsers } from '../../lib/use-streamers'
import PageHeader from '../components/PageHeader'
import { pairLabel, pairPeer, useMe } from '../../lib/use-me'
import dc from '@/app/ui/data-card.module.scss'
import CoverGenerator from './CoverGenerator'
import CoverFrames from './CoverFrames'
import { draftMedia, type DraftSource } from './draft-video'

const Players = dynamic(() => import('@/app/ui/Player'), { ssr: false })

type UploadJoin = {
  id: string
  filename: string
  created_at: number
  clip_count: number
  hardsubs_filename?: string
}

type DraftVideoItem = { key: string; label: string; source: DraftSource }

type UploadStatus =
  | { status: 'uploading' | 'submitting' | 'submitted' }
  | { status: 'failed'; message: string }

const videoExtensions = '.mp4,.flv,.avi,.wmv,.mov,.webm,.mpeg4,.ts,.mpg,.rm,.rmvb,.mkv,.m4v,.3gp'

function draftValues(template: StudioEntity) {
  return {
    ...template,
    tid: template.tid ?? 171,
    copyright: template.copyright ?? 2,
    title: template.title ?? '',
    description: template.description ?? '',
    tags: template.tags ?? [],
    sound: [template.dolby && 'dolby', template.hires && 'hires'].filter(Boolean),
    interaction: [template.up_close_danmu && 'up_close_danmu',
      template.up_close_reply && 'up_close_reply', template.up_selection_reply && 'up_selection_reply'].filter(Boolean),
    no_reprint: template.no_reprint === 1,
    is_only_self: template.is_only_self === 1,
    charging_pay: template.charging_pay === 1,
    isDtime: Boolean(template.dtime),
  }
}
type DraftValues = ReturnType<typeof draftValues>

function DraftVideo({ source }: { source: DraftSource }) {
  const [localUrl, setLocalUrl] = useState('')
  const [error, setError] = useState('')
  const file = source.kind === 'local' ? source.file : undefined
  useEffect(() => {
    if (!file) return
    const url = URL.createObjectURL(file)
    // Blob URLs are created only after mounting and revoked when switching files.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setLocalUrl(url)
    return () => URL.revokeObjectURL(url)
  }, [file])

  const { url, type } = draftMedia(source, localUrl)

  return (
    <>
      <div style={{ aspectRatio: '16 / 9', background: '#000', borderRadius: 8, overflow: 'hidden' }}>
        {url && (type
          ? <Players url={url} type={type} onError={setError} />
          : <video aria-label="稿件视频播放器" src={url} controls playsInline preload="metadata"
              style={{ display: 'block', width: '100%', height: '100%' }}
              onLoadedData={() => setError('')}
              onError={() => setError('无法加载视频，或浏览器不支持该格式。')} />)}
      </div>
      {error && <Banner type="warning" closeIcon={null} description={error} />}
    </>
  )
}

function DraftVideoPreview({ items, active, onSelect }: {
  items: DraftVideoItem[]; active: DraftVideoItem; onSelect: (key: string) => void
}) {
  return (
    <section aria-label="投稿视频预览" style={{ display: 'grid', gap: 8 }}>
      <strong>投稿视频预览</strong>
      {items.length > 1 ? (
        <select aria-label="选择预览分 P" value={active.key} onChange={event => onSelect(event.target.value)}
          style={{ width: '100%', padding: '8px 12px', borderRadius: 6, border: '1px solid var(--semi-color-border)',
            background: 'var(--semi-color-fill-0)', color: 'var(--semi-color-text-0)', font: 'inherit' }}>
          {items.map(({ key, label }, index) => <option key={key} value={key}>{index + 1}. {label}</option>)}
        </select>
      ) : <span style={{ overflowWrap: 'anywhere' }}>{active.label}</span>}
      <DraftVideo key={active.key} source={active.source} />
    </section>
  )
}

function DraftCover({ templatePath, file, keep, disabled, activeVideo, onChange }: {
  templatePath?: string; file?: File; keep: boolean; disabled: boolean
  activeVideo?: DraftVideoItem
  onChange: (file: File | undefined, keep: boolean) => void
}) {
  const [generating, setGenerating] = useState(false)
  const [captureVideo, setCaptureVideo] = useState<DraftVideoItem>()
  const picker = useRef<HTMLInputElement>(null)
  const image = useRef<HTMLImageElement>(null)
  useEffect(() => {
    if (!file || !image.current) return
    const preview = URL.createObjectURL(file)
    image.current.src = preview
    return () => URL.revokeObjectURL(preview)
  }, [file])

  return (
    <section aria-label="视频封面" style={{ display: 'grid', gap: 8, padding: '12px 0' }}>
      <strong>视频封面</strong>
      {file && (
        // A local blob URL is previewed directly in the browser.
        // eslint-disable-next-line @next/next/no-img-element
        <img ref={image} alt="封面预览" style={{ width: 280, maxWidth: '100%', maxHeight: 210, objectFit: 'contain', borderRadius: 8 }} />
      )}
      <span>{file?.name ?? (keep && templatePath ? `使用模板封面 · ${templatePath.split('/').pop()}` : '尚未选择封面')}</span>
      <input aria-label="选择封面文件" type="file" accept="image/jpeg,image/png,.jpg,.jpeg,.png" hidden ref={picker}
        onChange={event => {
          if (event.target.files?.[0]) onChange(event.target.files[0], keep)
          event.target.value = ''
        }} />
      <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8 }}>
        <Button onClick={() => picker.current?.click()}>从电脑选择封面</Button>
        <Button disabled={disabled || !activeVideo} onClick={() => setCaptureVideo(activeVideo)}>自动截取封面</Button>
        <Button disabled={disabled} onClick={() => setGenerating(true)}>生成封面</Button>
        {(file || (keep && templatePath)) && <Button onClick={() => onChange(undefined, false)}>移除封面</Button>}
        {templatePath && (file || !keep) && <Button onClick={() => onChange(undefined, true)}>恢复模板封面</Button>}
      </div>
      {captureVideo && (
        <CoverFrames source={captureVideo.source} label={captureVideo.label} onClose={() => setCaptureVideo(undefined)}
          onChoose={file => {
            onChange(file, keep)
            setCaptureVideo(undefined)
          }} />
      )}
      {generating && (
        <CoverGenerator onClose={() => setGenerating(false)} onChoose={file => {
          onChange(file, keep)
          setGenerating(false)
        }} />
      )}
    </section>
  )
}

export default function UploadManager() {
  const { Text } = Typography
  const [visible, setVisible] = useState(false)
  const router = useRouter()
  const { trigger: deleteUpload } = useSWRMutation('/v1/upload/streamers', requestDelete)
  const { data: templates, error, isLoading } = useSWR<StudioEntity[]>(
    '/v1/upload/streamers',
    fetcher
  )
  const { biliUsers } = useBiliUsers()
  const { me, can } = useMe()
  const canManageAccounts = can('account.manage')
  const canEditTemplates = can('template.edit')
  const canSubmit = can('upload.submit')
  // 本机加入了控制面时，控制面下发的投稿模板只读（后端对它们的改删返回 409）
  const fleet = me?.fleet_node
  const managedIds = new Set(fleet?.templates ?? [])
  const managedHint = fleet
    ? fleet.local
      ? '这是 Fleet 投稿模板，请到「节点 › 投稿模板」修改'
      : `由控制面 ${fleet.controller} 管理，请到控制面修改`
    : undefined
  // 一主一备里的节点：配对里的模板照常修改，改动与对端双向同步；配对里的直播间在用，删除仍返回 409
  const pair = fleet?.pair
  const pairedIds = new Set(pair?.templates ?? [])
  const pairHint = pair
    ? `在两台之间同步：这里的修改会同步到${pairPeer(pair)} ${fleet?.controller}，那边的修改也会同步过来`
    : undefined
  const pairDeleteHint = '配对里的直播间在用这个模板，不能在这里删除'

  const handleAddLinkClick = (event: React.MouseEvent) => {
    if (biliUsers.length === 0) {
      event.preventDefault()
      if (canManageAccounts) change()
      Notification.info({
        title: 'B 站账号列表为空',
        position: 'top',
        content: canManageAccounts
          ? '请先在右侧点击新增账号'
          : '请联系超级管理员先登记 B 站账号',
        duration: 3,
      })
    }
  }

  const change = () => setVisible(!visible)
  const onConfirm = async (id: number) => {
    await deleteUpload(id)
  }

  const [selectEntity, setSelectEntity] = useState<StudioEntity>()
  const [selected, setSelected] = useState<string[]>([])
  const [localFiles, setLocalFiles] = useState<{ id: string; file: File }[]>([])
  const [editing, setEditing] = useState(false)
  const [draft, setDraft] = useState<DraftValues>()
  const [cover, setCover] = useState<File>()
  const [keepTemplateCover, setKeepTemplateCover] = useState(true)
  const form = useRef<FormApi<DraftValues>>(undefined)
  const [submitting, setSubmitting] = useState(false)
  const [jobId, setJobId] = useState<string>()
  const upload = useSWR<UploadStatus>(jobId ? `/v1/uploads/jobs/${jobId}` : null, fetcher, {
    refreshInterval: status => status?.status === 'submitted' || status?.status === 'failed' ? 0 : 1500,
  })
  const status = upload.data?.status
  const busy = submitting || Boolean(jobId && status !== 'submitted' && status !== 'failed')
  const submitted = status === 'submitted'
  const [failure, setFailure] = useState('')
  const fileInput = useRef<HTMLInputElement>(null)
  const nextFileId = useRef(0)
  const recordings = useSWR<FileList[]>(selectEntity ? '/v1/videos?include_archived=true' : null, fetcher)
  const joins = useSWR<UploadJoin[]>(selectEntity ? '/v1/uploads/joins' : null, fetcher)

  const [activeVideoKey, setActiveVideoKey] = useState<string>()
  const sources = new Map<string, DraftSource>()
  const joinLabel = (join: UploadJoin) =>
    `${join.clip_count} 段 · ${new Date(join.created_at).toLocaleString('zh-CN')}`
  const data = [
    ...(joins.data ?? []).flatMap(join => {
      if (!join.hardsubs_filename) return []
      const key = 'hardsubs:' + join.id
      sources.set(key, { kind: 'hardsubs', id: join.id })
      return [{ key, value: key, label: `带内嵌字幕 · ${join.hardsubs_filename} · ${joinLabel(join)}` }]
    }),
    ...(joins.data ?? []).slice(0, 30).map(join => {
      const key = 'join:' + join.id
      sources.set(key, { kind: 'join', id: join.id })
      return { key, value: key, label: `${join.filename} · ${joinLabel(join)}` }
    }),
    ...localFiles.map(({ id, file }) => {
      sources.set(id, { kind: 'local', file })
      return { key: id, value: id, label: `${file.name} · 电脑文件` }
    }),
    ...(recordings.data ?? []).map(recording => {
      const key = 'recording:' + recording.name
      sources.set(key, { kind: 'recording', name: recording.name })
      return { key, value: key, label: recording.name }
    }),
  ]
  const selectedKeys = selected.filter(key => sources.has(key))

  const showDialog = (entity: StudioEntity) => {
    setSelected([])
    setActiveVideoKey(undefined)
    setLocalFiles([])
    setFailure('')
    setDraft(draftValues(entity))
    setCover(undefined)
    setKeepTemplateCover(true)
    setJobId(undefined)
    setEditing(false)
    setSelectEntity(entity)
  }

  const addLocalFiles = (files: ArrayLike<File>) => {
    const entries = Array.from(files, file => ({ id: 'local:' + nextFileId.current++, file }))
    setLocalFiles(current => [...current, ...entries])
    setSelected(current => [...current, ...entries.map(entry => entry.id)])
    setFailure('')
  }

  const handleOk = async () => {
    if (!selectEntity || !editing || busy || submitted || !form.current || selectedKeys.length === 0) return
    const api = form.current
    setSubmitting(true)
    setFailure('')
    try {
      const values = await api.validate().catch(() => undefined)
      if (!values?.title?.trim()) return
      setJobId(undefined)
      const chosen = selectedKeys.map(key => sources.get(key)!)
      const body = new FormData()
      body.append('selection', JSON.stringify({
        template_id: selectEntity.id,
        details: {
          title: values.title.trim(), tags: values.tags ?? [], description: values.description ?? '',
          settings: {
            copyright: values.copyright, copyright_source: values.copyright_source ?? '',
            tid: values.tid, tid_v2: values.tid_v2 ?? null,
            dynamic: values.dynamic ?? '', dtime: values.isDtime ? values.dtime || 14400 : null,
            dolby: values.sound?.includes('dolby') ?? false, hires: values.sound?.includes('hires') ?? false,
            no_reprint: Boolean(values.no_reprint), is_only_self: Boolean(values.is_only_self),
            charging_pay: Boolean(values.charging_pay),
            up_close_danmu: values.interaction?.includes('up_close_danmu') ?? false,
            up_close_reply: values.interaction?.includes('up_close_reply') ?? false,
            up_selection_reply: values.interaction?.includes('up_selection_reply') ?? false,
            credits: (values.credits ?? []).map(credit => ({ username: credit.username, uid: Number(credit.uid) })),
            use_template_cover: keepTemplateCover,
          },
        },
        sources: chosen.map(source => source.kind === 'local' ? { kind: 'local' } : source),
      }))
      for (const source of chosen) {
        if (source.kind === 'local') body.append('local', source.file)
      }
      if (cover) body.append('cover', cover)
      const response = await proxy('/v1/uploads/files', { method: 'POST', headers: { 'x-biliup-upload': '1' }, body })
      const result: { id: string } = await response.json()
      setJobId(result.id)
    } catch (error) {
      setFailure(error instanceof Error ? error.message : String(error))
    } finally {
      setSubmitting(false)
    }
  }

  if (editing && selectEntity && draft) {
    const items = selectedKeys.map(key => ({
      key,
      label: data.find(item => item.key === key)!.label,
      source: sources.get(key)!,
    }))
    const activeVideo = items.find(item => item.key === activeVideoKey) ?? items[0]
    return (
      <>
        <PageHeader icon={<IconSendStroked size="large" />} title="编辑投稿"
          description={`使用「${selectEntity.template_name}」模板`} />
        <div className={dc.content}>
          <div className={dc.card} style={{ padding: 24, maxWidth: 760, display: 'grid', gap: 14 }}>
            {activeVideo && <DraftVideoPreview items={items} active={activeVideo} onSelect={setActiveVideoKey} />}
            <fieldset disabled={busy || submitted} style={{ border: 0, padding: 0, margin: 0, minWidth: 0 }}>
              <Form<DraftValues> initValues={draft} getFormApi={api => { form.current = api }}
                onValueChange={values => setDraft(values)} labelPosition="top">
                {props => <TemplateFields {...props} submission coverField={
                  <DraftCover templatePath={selectEntity.cover_path} file={cover} keep={keepTemplateCover}
                    disabled={busy || submitted} activeVideo={activeVideo}
                    onChange={(file, keep) => {
                      if (file && file.size > 5 * 1024 * 1024) {
                        setFailure('封面不能超过 5 MB。')
                        return
                      }
                      setFailure('')
                      setCover(file)
                      setKeepTemplateCover(keep)
                    }} />
                } />}
              </Form>
            </fieldset>
            {failure && <Banner type="danger" closeIcon={null} description={failure} />}
            {upload.data?.status === 'failed' && <Banner type="danger" closeIcon={null} description={upload.data.message} />}
            {upload.error && <Banner type="warning" closeIcon={null}
              description="暂时无法读取投稿状态。请到 B 站稿件管理或本机实时日志确认结果。" />}
            {submitted && <Banner type="success" closeIcon={null} description="B 站已接收稿件，可前往「B站稿件」查看审核状态。" />}
            {busy && !upload.error && <p role="status">
              {submitting ? '正在准备视频文件…' : status === 'submitting' ? '视频已上传，正在向 B 站提交稿件…' : '正在向 B 站上传视频…'}
            </p>}
            <div style={{ display: 'flex', gap: 10, flexWrap: 'wrap' }}>
              <Button disabled={busy && !upload.error} onClick={() => {
                setEditing(false)
                setJobId(undefined)
                setFailure('')
                if (submitted || upload.error) setSelectEntity(undefined)
              }}>{submitted || upload.error ? '返回投稿管理' : '返回选择文件'}</Button>
              {!submitted && <Button theme="solid" disabled={busy || selectedKeys.length === 0}
                loading={busy && !upload.error} onClick={() => void handleOk()}>确认投稿</Button>}
            </div>
          </div>
        </div>
      </>
    )
  }

  const actions = (
    <>
      {canManageAccounts && (
        <Button
          onClick={change}
          type="tertiary"
          icon={<IconUserListStroked />}
          aria-label="B 站账号"
          title="B 站账号"
        />
      )}
      {canEditTemplates && (
        <Link href="/upload-manager/add" prefetch={false} onClick={handleAddLinkClick}>
          <Button icon={<IconPlusCircle />} theme="solid">
            新建
          </Button>
        </Link>
      )}
    </>
  )

  return (
    <>
      {canManageAccounts && <UserList visible={visible} onCancel={change} />}
      <Modal
        size="medium"
        title={`使用「${selectEntity?.template_name ?? ''}」模板投稿`}
        okText="下一步：编辑稿件"
        style={{ width: 'min(680px, 90vw)' }}
        visible={Boolean(selectEntity)}
        confirmLoading={submitting}
        okButtonProps={{ disabled: selectedKeys.length === 0 }}
        cancelButtonProps={{ disabled: submitting }}
        onOk={() => setEditing(true)}
        onCancel={() => { if (!submitting) setSelectEntity(undefined) }}
        bodyStyle={{ overflow: 'auto' }}
        closeOnEsc={!submitting}
        closable={!submitting}
        maskClosable={false}
      >
        <div style={{ display: 'flex', gap: 12, alignItems: 'center', flexWrap: 'wrap', marginBottom: 12 }}>
          <input ref={fileInput} type="file" multiple accept={videoExtensions} hidden
            onChange={event => {
              if (event.target.files) addLocalFiles(event.target.files)
              event.target.value = ''
            }}
          />
          <Button disabled={submitting} onClick={() => fileInput.current?.click()}>从电脑选择视频</Button>
          <Text type="secondary">勾选带字幕成片、拼接原版或录像，右侧可拖动调整分 P 顺序。</Text>
        </div>
        {(joins.error || recordings.error) && (
          <Banner type="warning" closeIcon={null} description={
            `${joins.error ? '最近拼接视频加载失败。' : ''}${recordings.error ? '录像列表加载失败。' : ''}`
          } />
        )}
        {failure && <Banner type="danger" closeIcon={null} description={failure} />}
        {submitting && <p role="status">正在提交文件，请保持页面打开。</p>}
        {data.length === 0 && (
          <p>{joins.isLoading || recordings.isLoading
            ? '正在读取服务器上的视频…'
            : '暂无可选的服务器视频，可以从电脑选择。'}</p>
        )}
        <Transfer
          style={{ height: 380 }}
          dataSource={data}
          draggable={!submitting}
          disabled={submitting}
          value={selectedKeys}
          onChange={values => setSelected(values.map(String))}
        />
      </Modal>

      <PageHeader
        icon={<IconCloudStroked size="large" />}
        title="投稿管理"
        description={
          canEditTemplates ? '管理上传模板，用拼接视频、录制文件或电脑视频投稿' : '查看已配置的上传模板'
        }
        actions={canEditTemplates || canManageAccounts ? actions : undefined}
      />
      <div className={dc.content}>
        {pair && fleet ? (
          <Banner
            type="info"
            fullMode={false}
            closeIcon={null}
            style={{ marginBottom: 12 }}
            description={
              `本机与 ${fleet.controller} 组成一主一备：标着「${pairLabel(pair)}」的 ${pairedIds.size} 个模板是配对里的直播间在用的，在两台之间双向同步，在哪台修改都可以，同一项两边都改过时以后改的为准。B 站账号也在两台之间同步，在哪台登录都行。` +
              (managedIds.size > 0
                ? `标着「托管」的 ${managedIds.size} 个模板由控制面 ${fleet.controller} 管理，这里只能查看和用来投稿。`
                : '')
            }
          />
        ) : fleet ? (
          <Banner
            type="info"
            fullMode={false}
            closeIcon={null}
            style={{ marginBottom: 12 }}
            description={
              fleet.local
                ? managedIds.size > 0
                  ? `标着「托管」的 ${managedIds.size} 个模板是随房间分派到本机的 Fleet 投稿模板，这里只能查看和用来投稿，修改请到「节点 › 投稿模板」。这里自己的模板不受影响。`
                  : '已启用「本机」节点：Fleet 投稿模板会随房间分派到这里并标为「托管」，请到「节点 › 投稿模板」修改。'
                : managedIds.size > 0
                  ? `标着「托管」的 ${managedIds.size} 个模板由控制面 ${fleet.controller} 管理，这里只能查看和用来投稿，修改请到控制面。本机自己的模板不受影响。`
                  : `本机已加入控制面 ${fleet.controller}；控制面下发的投稿模板会由控制面 ${fleet.controller} 管理，这里只能查看。`
            }
          />
        ) : null}
        <List
          grid={{
            gutter: 12,
            xs: 24,
            sm: 24,
            md: 12,
            lg: 8,
            xl: 6,
            xxl: 4,
          }}
          dataSource={templates}
          loading={isLoading}
          renderItem={(item: StudioEntity) => (
            <List.Item>
              <Card
                shadows="hover"
                style={{
                  margin: '8px 2px',
                  flexGrow: 1,
                  height: '100%',
                  borderRadius: 12,
                  border: '1px solid var(--semi-color-border)',
                }}
                bodyStyle={{
                  display: 'flex',
                  alignItems: 'center',
                  justifyContent: 'space-between',
                  gap: 12,
                  padding: '16px 18px',
                }}
              >
                {/* 模板名称:优先完整展示,占满剩余宽度,超出才尾部省略 */}
                <Text
                  ellipsis={{ showTooltip: true }}
                  title={item.template_name}
                  style={{ flex: 1, minWidth: 0, fontSize: 14, fontWeight: 600 }}
                >
                  {item.template_name}
                </Text>
                {managedIds.has(item.id) ? (
                  <Tooltip content={managedHint}>
                    <Tag size="small" color="violet" style={{ flexShrink: 0 }}>
                      托管
                    </Tag>
                  </Tooltip>
                ) : null}
                {pair && pairedIds.has(item.id) ? (
                  <Tooltip content={pairHint}>
                    <Tag size="small" color="green" style={{ flexShrink: 0 }}>
                      {pairLabel(pair)}
                    </Tag>
                  </Tooltip>
                ) : null}
                {(canSubmit || canEditTemplates) && (
                  <ButtonGroup style={{ flexShrink: 0 }} theme="borderless">
                    {[
                      canSubmit && (
                        <Button
                          key="send"
                          icon={<IconSendStroked />}
                          aria-label="投稿"
                          title="使用此模板投稿"
                          onClick={() => showDialog(item)}
                        />
                      ),
                      canEditTemplates && (
                        <Button
                          key="edit"
                          icon={<IconEdit2Stroked />}
                          aria-label="编辑"
                          disabled={managedIds.has(item.id)}
                          title={managedIds.has(item.id) ? managedHint : undefined}
                          onClick={() => router.push(`/upload-manager/edit?id=${item.id}`)}
                        />
                      ),
                      canEditTemplates && (
                        <Popconfirm
                          key="delete"
                          title="确定是否要删除？"
                          content="此操作将不可逆"
                          margin={50}
                          disabled={managedIds.has(item.id) || pairedIds.has(item.id)}
                          onConfirm={async () => await onConfirm(item.id)}
                        >
                          <Button
                            theme="borderless"
                            icon={<IconDeleteStroked />}
                            aria-label="删除"
                            disabled={managedIds.has(item.id) || pairedIds.has(item.id)}
                            title={
                              managedIds.has(item.id)
                                ? managedHint
                                : pairedIds.has(item.id)
                                  ? pairDeleteHint
                                  : undefined
                            }
                          />
                        </Popconfirm>
                      ),
                    ].filter(Boolean)}
                  </ButtonGroup>
                )}
              </Card>
            </List.Item>
          )}
        />
      </div>
    </>
  )
}
