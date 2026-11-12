'use client'
import { Button, Modal, Table, Toast, Typography } from '@douyinfe/semi-ui'
import { SortOrder } from '@douyinfe/semi-ui/lib/es/table'
import useSWR, { useSWRConfig } from 'swr'
import { API_BASE, fetcher, proxy, type FileList } from '@/app/lib/api-streamer'
import { useState } from 'react'
import dynamic from 'next/dynamic'
import Link from 'next/link'
import { humDate } from '@/app/lib/utils'
import { formatSize } from '@/app/lib/use-dashboard'
import { replayHref, sessionUrl } from '@/app/lib/sessions'
import { useMe } from '@/app/lib/use-me'
import dc from '@/app/ui/data-card.module.scss'
import styles from './page.module.scss'
import TrashConfirmation from './TrashConfirmation'

const Players = dynamic(() => import('@/app/ui/Player'), {
  ssr: false,
})

/** 「录制文件」：已录完的文件在线回放；属于某一场的文件能按场次打开回看 */
export default function FilesTab() {
  const { Text } = Typography
  const { data, error, isLoading, mutate: refreshFiles } = useSWR<FileList[]>(
    '/v1/videos?include_archived=true',
    fetcher,
  )
  const { mutate } = useSWRConfig()
  const { can } = useMe()
  const canDelete = can('file.delete')
  const [fileName, setFileName] = useState<string>()
  const [visible, setVisible] = useState(false)
  const [deleteTarget, setDeleteTarget] = useState<FileList>()
  const [deleting, setDeleting] = useState(false)

  const confirmDelete = async () => {
    if (!deleteTarget || deleting) return
    setDeleting(true)
    try {
      const { name, updateTime, size, session_id } = deleteTarget
      await proxy('/v1/videos', {
        method: 'DELETE',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name, updateTime, size }),
      })
      setDeleteTarget(undefined)
      Toast.success('已移入废纸篓')
      void mutate('/v1/videos')
      if (session_id !== undefined) {
        void mutate(sessionUrl(session_id))
        void mutate(`${sessionUrl(session_id)}/recordings`)
      }
    } catch (error) {
      Toast.error(error instanceof Error ? error.message : '无法将录制文件移入废纸篓')
    } finally {
      setDeleting(false)
      void refreshFiles()
    }
  }

  const columns = [
    {
      title: '标题',
      dataIndex: 'name',
      render: (text: any) => <Text strong>{text}</Text>,
    },
    {
      title: '大小',
      dataIndex: 'size',
      render: (size: number) => formatSize(size || 0),
    },
    {
      title: '更新日期',
      dataIndex: 'updateTime',
      defaultSortOrder: 'descend' as SortOrder,
      sorter: (a: any, b: any) => (a.updateTime - b.updateTime > 0 ? 1 : -1),
      render: (time: number) => humDate(time),
    },
    {
      title: '',
      dataIndex: 'operate',
      render: (text: any, record: FileList) => (
        <span className={styles.actions}>
          <Text link style={{ cursor: 'pointer' }} onClick={() => showDialog(record.name)}>
            播放
          </Text>
          {record.session_id !== undefined ? (
            <Link
              href={replayHref(record.session_id, record.segment_start_ms)}
              className={styles.replayLink}
              title="打开这一场的回看页，定位到这个文件的开头；可以拖到整场任意位置"
            >
              按场次回看
            </Link>
          ) : null}
          {canDelete && (
            <Button
              size="small"
              type="danger"
              theme="borderless"
              onClick={() => setDeleteTarget(record)}
            >
              删除
            </Button>
          )}
        </span>
      ),
    },
  ]

  const showDialog = (name: string) => {
    setVisible(true)
    setFileName(name)
  }

  return (
    <>
      <div className={dc.card}>
        <Table
          size="small"
          rowKey="name"
          scroll={{ x: 'max-content' }}
          columns={columns}
          dataSource={data}
          loading={isLoading}
          empty={error ? '加载失败，请检查后端连接' : '暂无数据'}
        />
      </div>
      <TrashConfirmation
        visible={deleteTarget !== undefined}
        title="将录播移入废纸篓？"
        pending={deleting}
        onConfirm={confirmDelete}
        onCancel={() => setDeleteTarget(undefined)}
      >
        <p style={{ overflowWrap: 'anywhere' }}>{deleteTarget?.name}</p>
        <p>此文件将移入运行 biliup 的电脑的废纸篓。</p>
      </TrashConfirmation>
      <Modal
        visible={visible}
        onCancel={() => setVisible(false)}
        closeOnEsc={true}
        style={{ width: 'min(600px, 90vw)' }}
        size="large"
        bodyStyle={{ height: 500 }}
        footer={null}
      >
        {fileName && <Players url={API_BASE + '/static/' + encodeURIComponent(fileName)} />}
        <div id="mse" />
      </Modal>
    </>
  )
}
