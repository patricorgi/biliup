'use client'
import { Button, Spin, Table, Toast, Tooltip, Typography } from '@douyinfe/semi-ui'
import { SortOrder } from '@douyinfe/semi-ui/lib/es/table'
import Link from 'next/link'
import useSWR, { useSWRConfig } from 'swr'
import { useState } from 'react'
import { fetcher, type FileList, type StreamerInfo } from '@/app/lib/api-streamer'
import { isReadable, replayHref, type SessionDetail, sessionUrl } from '@/app/lib/sessions'
import { humDate } from '@/app/lib/utils'
import { type AutoClipAvailability, useAutoClip, useSessionJob } from '@/app/lib/auto-clip'
import { JobCell } from '@/app/ui/auto-clip/AutoClipJob'
import Filter from './Filter'
import { useIsMobile } from '../../lib/useIsMobile'
import dc from '@/app/ui/data-card.module.scss'
import styles from './sessions.module.scss'
import { useMe } from '@/app/lib/use-me'
import TrashConfirmation from './TrashConfirmation'

type RecordingFile = Pick<FileList, 'name' | 'updateTime' | 'size'>

type SessionRecordings = {
  recording: boolean
  files: RecordingFile[]
}

type TrashSelection = {
  session: StreamerInfo
  all: boolean
  files: RecordingFile[]
}

const recordingsUrl = (id: number) => `${sessionUrl(id)}/recordings`
const errorMessage = (error: unknown) => error instanceof Error ? error.message : '录像操作失败'


/** 「直播场次」：每场一行，能按场次回看的点「回看」进回看页 */
export default function SessionsTab() {
  const isMobile = useIsMobile()
  const { Text } = Typography
  const { data: data, error, isLoading } = useSWR<any[]>('/v1/streamer-info', fetcher)
  const autoClip = useAutoClip()
  const { can } = useMe()
  const canDelete = can('file.delete')
  const { mutate } = useSWRConfig()
  const [selection, setSelection] = useState<TrashSelection>()
  const [preparing, setPreparing] = useState<number>()
  const [pending, setPending] = useState(false)

  const prepareSession = async (session: StreamerInfo) => {
    setPreparing(session.id)
    try {
      const recordings: SessionRecordings = await fetcher(recordingsUrl(session.id))
      if (recordings.recording) {
        Toast.warning('直播仍在录制，请等录制结束')
      } else {
        setSelection({ session, all: true, files: recordings.files })
      }
    } catch (error) {
      Toast.error(errorMessage(error))
    } finally {
      setPreparing(undefined)
    }
  }

  const confirmTrash = async () => {
    if (!selection || pending) return
    setPending(true)
    const { session, all, files } = selection
    try {
      const result: { moved: number; error: string | null; session_removed: boolean } = await fetcher(recordingsUrl(session.id), {
        method: 'DELETE',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ all, files }),
      })
      if (result.error) {
        Toast.error(result.moved > 0
          ? `已移入废纸篓 ${result.moved} 个文件。${result.error}`
          : result.error)
      } else if (result.session_removed) {
        await mutate('/v1/streamer-info', (rows: StreamerInfo[] | undefined) =>
          rows?.filter(row => row.id !== session.id))
        Toast.success('已删除场次')
      } else {
        Toast.success(`已将 ${result.moved} 个录制文件移入废纸篓`)
      }
    } catch (error) {
      Toast.error(errorMessage(error))
    } finally {
      setSelection(undefined)
      setPending(false)
      void mutate(recordingsUrl(session.id))
      void mutate(sessionUrl(session.id))
      void mutate('/v1/videos')
      void mutate('/v1/videos?include_archived=true')
    }
  }

  if (isLoading) {
    return (
      <div style={{ padding: '80px 0', textAlign: 'center' }}>
        <Spin size="large" />
      </div>
    )
  }

  const columns = [
    {
      title: '名称',
      dataIndex: 'name',
      onFilter: (value: any, record: any) => record.name.includes(value),
      renderFilterDropdown: Filter,
    },
    {
      title: '标题',
      dataIndex: 'title',
      render: (text: any) => (
        <Text strong style={{ whiteSpace: 'nowrap' }}>
          {text}
        </Text>
      ),
      onFilter: (value: any, record: any) => record.title.includes(value),
      renderFilterDropdown: Filter,
    },
    ...(autoClip.visible
      ? [
          {
            title: '自动切片',
            dataIndex: 'auto_clip',
            render: (_: unknown, record: StreamerInfo) => (
              <AutoClipCell record={record} availability={autoClip} />
            ),
          },
        ]
      : []),
    ...(isMobile
      ? []
      : [
          {
            title: '链接',
            dataIndex: 'url',
          },
          {
            title: '封面',
            dataIndex: 'live_cover_path',
          },
        ]),
    {
      title: '更新日期',
      dataIndex: 'date',
      defaultSortOrder: 'descend' as SortOrder,
      sorter: (a: any, b: any) => (a.date - b.date > 0 ? 1 : -1),
      render: (time: number) => humDate(time),
    },
    {
      title: '',
      dataIndex: 'replay',
      fixed: 'right' as const,
      width: canDelete ? 132 : 72,
      render: (_: unknown, record: StreamerInfo) => (
        <span className={styles.actions}>
          {(record.has_timeline || record.has_legacy_recordings) ? (
            <Link href={replayHref(record.id)} className={styles.replayLink}>
              回看
            </Link>
          ) : (
            <Tooltip content="这场直播没有可用的回看时间轴">
              <Text type="tertiary" className={styles.replayOff}>回看</Text>
            </Tooltip>
          )}
          {canDelete && (
            <Button
              size="small"
              type="danger"
              theme="borderless"
              disabled={preparing !== undefined && preparing !== record.id}
              loading={preparing === record.id}
              onClick={() => prepareSession(record)}
            >
              删除
            </Button>
          )}
        </span>
      ),
    },
  ]

  return (
    <div className={dc.card}>
      <Table
        size="small"
        rowKey="id"
        scroll={{ x: 'max-content' }}
        columns={columns}
        dataSource={data}
        expandedRowRender={(record) => record && (
          <FileLists
            record={record}
            onTrash={canDelete ? (file) => setSelection({ session: record, all: false, files: [file] }) : undefined}
          />
        )}
        empty={error ? '加载失败，请检查后端连接' : '暂无数据'}
      />
      <TrashConfirmation
        visible={selection !== undefined}
        title={selection?.all ? '删除直播场次？' : '将录播移入废纸篓？'}
        confirmText={selection?.all ? '删除场次' : '移入废纸篓'}
        pending={pending}
        onConfirm={confirmTrash}
        onCancel={() => setSelection(undefined)}
      >
        {selection?.all && <p>{selection.session.name} · {humDate(selection.session.date)} · 共 {selection.files.length} 个录制文件</p>}
        {selection?.all && (
          <p>{selection.files.length > 0
            ? '录像将移入废纸篓，场次也会从历史中移除。'
            : '这场直播已没有录制文件，确认后将从历史中移除。'}</p>
        )}
        {!!selection?.files.length && (
          <ul className={styles.trashFiles}>
            {selection.files.map(file => <li key={file.name}>{file.name}</li>)}
          </ul>
        )}
      </TrashConfirmation>
    </div>
  )
}

/** 只在当前页渲染，当前页各行的任务合成一个请求；没有时间轴的场次不能生成 */
function AutoClipCell({ record, availability }: { record: StreamerInfo; availability: AutoClipAvailability }) {
  const { data, isLoading } = useSessionJob(record.has_timeline ? record.id : null)
  if (!record.has_timeline) {
    return (
      <Tooltip content="这一场在升级前录制，没有时间轴，不能生成候选">
        <Typography.Text type="tertiary" className={styles.replayOff}>
          —
        </Typography.Text>
      </Tooltip>
    )
  }
  return <JobCell sessionId={record.id} job={data ?? null} availability={availability} loading={isLoading} />
}

const baseName = (path: string) => path.split(/[\\/]/).pop() ?? path

// 展开子行:该次直播的录制文件列表；有时间轴的场次，能回看的文件可以直接定位到这一段
function FileLists({
  record, onTrash,
}: {
  record: StreamerInfo
  onTrash?: (file: RecordingFile) => void
}) {
  const { data, error, isLoading } = useSWR<SessionRecordings>(recordingsUrl(record.id), fetcher)
  const { data: detail } = useSWR<SessionDetail>(
    record.has_timeline || record.has_legacy_recordings ? sessionUrl(record.id) : null,
    fetcher,
    {
      revalidateOnFocus: false,
      shouldRetryOnError: false,
    }
  )
  const startOf = new Map(
    detail?.segments.filter(isReadable).map(seg => [seg.file_name, seg.start_ms]) ?? []
  )

  if (isLoading) return <div>加载中...</div>
  if (error) return <div>录像列表加载失败</div>
  if (!data?.files.length) return <div>暂无录制文件</div>

  return (
    <div className={styles.files}>
      文件列表
      {data.files.map(file => {
        const name = baseName(file.name)
        const start = startOf.get(name)
        return (
          <div key={file.name} className={styles.file}>
            <span>{name}</span>
            {start !== undefined && (
              <Link href={replayHref(record.id, start)} className={styles.fileReplay}>
                回看这一段
              </Link>
            )}
            {onTrash && (
              <Button
                size="small"
                type="danger"
                theme="borderless"
                disabled={data.recording}
                title={data.recording ? '请等录制结束' : undefined}
                onClick={() => onTrash(file)}
              >
                删除
              </Button>
            )}
          </div>
        )
      })}
    </div>
  )
}
