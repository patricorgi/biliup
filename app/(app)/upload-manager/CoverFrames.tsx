'use client'

import { Banner, Button, Modal } from '@douyinfe/semi-ui'
import { useEffect, useState } from 'react'
import { captureFrames, framesPerBatch } from './capture-frames'
import type { DraftSource } from './draft-video'
import styles from './cover-frames.module.scss'

type Candidate = { time: string; file: File; url: string }

function timestamp(seconds: number) {
  const value = Math.round(seconds * 1000)
  const hours = Math.floor(value / 3_600_000)
  const minutes = Math.floor(value / 60_000) % 60
  const wholeSeconds = Math.floor(value / 1000) % 60
  const clock = [hours, minutes, wholeSeconds].map(part => String(part).padStart(2, '0')).join(':')
  return clock + '.' + String(value % 1000).padStart(3, '0')
}

export default function CoverFrames({ source, label, onClose, onChoose }: {
  source: DraftSource
  label: string
  onClose: () => void
  onChoose: (file: File) => void
}) {
  const [candidates, setCandidates] = useState<Candidate[]>([])
  const [batch, setBatch] = useState(0)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState('')
  useEffect(() => {
    const controller = new AbortController()
    const urls: string[] = []
    void captureFrames(source, batch, controller.signal, ({ time, blob }) => {
      const displayTime = timestamp(time)
      const file = new File([blob], `封面-${displayTime.replaceAll(':', '-')}.jpg`, { type: 'image/jpeg' })
      const url = URL.createObjectURL(file)
      urls.push(url)
      setCandidates(current => [...current, { time: displayTime, file, url }])
    }).catch(error => {
      if (!controller.signal.aborted) {
        setError(error instanceof Error ? error.message : '无法截取封面。')
      }
    }).finally(() => {
      if (!controller.signal.aborted) setLoading(false)
    })
    return () => {
      controller.abort()
      for (const url of urls) URL.revokeObjectURL(url)
    }
  }, [source, batch])

  return (
    <Modal title="自动截取封面" visible onCancel={onClose} maskClosable={false}
      style={{ width: 'min(860px, 94vw)' }} bodyStyle={{ maxHeight: '70vh', overflowY: 'auto' }}
      footer={<div className={styles.footer}>
        <Button onClick={onClose}>关闭</Button>
        <Button theme="solid" disabled={loading} onClick={() => {
          setCandidates([])
          setError('')
          setLoading(true)
          setBatch(current => current + 1)
        }}>刷新候选</Button>
      </div>}>
      <p className={styles.source}>{label}</p>
      {loading && <p role="status">正在截取封面… {candidates.length} / {framesPerBatch}</p>}
      {error && <Banner type="warning" closeIcon={null} description={error} />}
      {candidates.length > 0 && <div className={styles.grid}>
        {candidates.map(({ time, file, url }) => (
          <figure key={url}>
            {/* eslint-disable-next-line @next/next/no-img-element */}
            <img alt={`${time} 的候选封面`} src={url} />
            <figcaption>
              <span>{time}</span>
              <div className={styles.actions}>
                <a href={url} download={file.name}>下载 JPG</a>
                <Button size="small" onClick={() => onChoose(file)}>用作封面</Button>
              </div>
            </figcaption>
          </figure>
        ))}
      </div>}
    </Modal>
  )
}
