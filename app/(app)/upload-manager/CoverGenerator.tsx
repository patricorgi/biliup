'use client'

import { Banner, Button, Input, Modal, TextArea } from '@douyinfe/semi-ui'
import { useEffect, useRef, useState } from 'react'
import useSWR from 'swr'
import { fetcher, proxy } from '../../lib/api-streamer'
import styles from './cover-generator.module.scss'

const endpoint = '/v1/uploads/cover-generation'
const referenceFormats = ['image/jpeg', 'image/png', 'image/webp']
const maxReferences = 8
const maxImageBytes = 5 * 1024 * 1024

const defaultLayout = '制作一张 B 站视频封面，输出 1536 × 864 的横版 JPG。让主体占据画面主要部分，缩成手机缩略图时也能一眼看清。主体、表情和重要文字集中在画面中央，两侧各留至少 12.5% 的裁切余量，兼顾 16:9 和居中裁切的 4:3。背景简洁，主体与背景有清晰对比。如需加字，按描述中的原文绘制醒目的简体中文，保留呼吸空间。没有指定封面文字时，画面保持无字。'

type Reference = { id: number; file: File }
type GeneratedCover = { file: File; url: string }

export default function CoverGenerator({ onClose, onChoose }: {
  onClose: () => void
  onChoose: (file: File) => void
}) {
  const configuration = useSWR<{ configured: boolean }>(endpoint, fetcher, { revalidateOnFocus: false })
  const [layout, setLayout] = useState(defaultLayout)
  const [description, setDescription] = useState('')
  const [key, setKey] = useState('')
  const [references, setReferences] = useState<Reference[]>([])
  const [result, setResult] = useState<GeneratedCover>()
  const [generating, setGenerating] = useState(false)
  const [error, setError] = useState('')
  const picker = useRef<HTMLInputElement>(null)
  const nextReference = useRef(0)
  const request = useRef<AbortController | null>(null)
  const needsKey = configuration.data?.configured === false

  useEffect(() => () => request.current?.abort(), [])
  useEffect(() => {
    if (result) return () => URL.revokeObjectURL(result.url)
  }, [result])

  const addReferences = (files: File[]) => {
    if (references.length + files.length > maxReferences) {
      setError('最多可添加 8 张参考图。')
      return
    }
    if (files.some(file => !referenceFormats.includes(file.type))) {
      setError('参考图须为 JPG、PNG 或 WebP。')
      return
    }
    if (files.some(file => file.size === 0 || file.size > maxImageBytes)) {
      setError('每张参考图需小于或等于 5 MB，并且不能为空。')
      return
    }
    const added = files.map(file => ({ id: nextReference.current++, file }))
    setReferences(current => [...current, ...added])
    setError('')
  }

  const generate = async () => {
    if (!configuration.data || request.current) return
    if (needsKey && !window.isSecureContext) {
      setError('请通过本机地址或 HTTPS 打开 biliup，再输入 API 密钥。')
      return
    }
    const controller = new AbortController()
    request.current = controller
    setGenerating(true)
    setError('')
    const prompt = [
      layout.trim(),
      description.trim() || '以所附参考图为主题和风格参考，构图适合视频封面。',
    ].filter(Boolean).join('\n\n')
    const body = new FormData()
    body.append('prompt', prompt)
    if (needsKey) body.append('api_key', key.trim())
    for (const reference of references) body.append('reference', reference.file)
    try {
      const response = await proxy(endpoint, {
        method: 'POST',
        headers: { 'x-biliup-cover': '1' },
        body,
        signal: controller.signal,
      })
      const blob = await response.blob()
      if (blob.type !== 'image/jpeg' || !blob.size || blob.size > maxImageBytes) {
        throw new Error('服务器返回了无效的封面。')
      }
      if (!controller.signal.aborted) {
        const file = new File([blob], '生成封面.jpg', { type: 'image/jpeg' })
        setResult({ file, url: URL.createObjectURL(file) })
      }
    } catch (error) {
      if (!controller.signal.aborted) setError(error instanceof Error ? error.message : '封面生成失败。')
    } finally {
      request.current = null
      if (!controller.signal.aborted) setGenerating(false)
    }
  }

  return (
    <Modal
      title="生成视频封面"
      visible
      onCancel={onClose}
      maskClosable={false}
      style={{ width: 'min(720px, 94vw)' }}
      bodyStyle={{ maxHeight: '68vh', overflowY: 'auto' }}
      footer={
        <div className={styles.actions}>
          <Button onClick={onClose}>关闭</Button>
          <Button
            onClick={() => void generate()}
            loading={generating}
            disabled={generating || !configuration.data || (needsKey && !key.trim())
              || (!description.trim() && references.length === 0)}
            theme={result ? 'light' : 'solid'}
          >{result ? '重新生成' : '生成封面'}</Button>
          {result && <Button theme="solid" disabled={generating} onClick={() => onChoose(result.file)}>用作封面</Button>}
        </div>
      }
    >
      <div className={styles.form}>
        {needsKey && (
          <label className={styles.field}>
            <span>OpenAI API 密钥</span>
            <Input aria-label="OpenAI API 密钥" type="password" value={key} onChange={setKey}
              maxLength={1024} autoComplete="off" disabled={generating} />
            <small>密钥仅在本窗口使用，关闭后清除。</small>
          </label>
        )}
        {configuration.error && <Banner type="warning" closeIcon={null}
          description="无法读取封面生成设置，请重新打开此窗口。" />}
        <label className={styles.field}>
          <span>封面描述</span>
          <TextArea aria-label="封面描述" value={description} onChange={setDescription} maxCount={1800}
            placeholder="例如：用浅色背景突出画面主体，在上方添加醒目的「精彩片段」标题。"
            autosize={{ minRows: 3, maxRows: 6 }} disabled={generating} />
        </label>
        <label className={styles.field}>
          <span>格式和构图提示词</span>
          <TextArea aria-label="格式和构图提示词" value={layout} onChange={setLayout} maxCount={1800}
            autosize={{ minRows: 3, maxRows: 6 }} disabled={generating} />
        </label>
        <div className={styles.field}>
          <div className={styles.references}>
            <Button disabled={generating || references.length >= maxReferences}
              onClick={() => picker.current?.click()}>添加参考图</Button>
            <small>可选 · JPG、PNG、WebP · 每张最多 5 MB</small>
          </div>
          <input type="file" aria-label="选择参考图" ref={picker} hidden multiple
            accept="image/jpeg,image/png,image/webp"
            onChange={event => {
              addReferences(Array.from(event.target.files ?? []))
              event.target.value = ''
            }} />
          {references.length > 0 && (
            <ul className={styles.files}>
              {references.map(({ id, file }) => (
                <li key={id}>
                  <span>{file.name}</span>
                  <Button size="small" type="tertiary" theme="borderless" disabled={generating}
                    aria-label={`移除参考图 ${file.name}`}
                    onClick={() => {
                      setReferences(current => current.filter(item => item.id !== id))
                      setError('')
                    }}>移除</Button>
                </li>
              ))}
            </ul>
          )}
        </div>
        <small>点击生成后，提示词和参考图将发送给 OpenAI，并按 API 用量计费。</small>
        {generating && <div role="status">正在生成封面，可能需要几分钟。</div>}
        {error && <Banner type="danger" closeIcon={null} description={error} />}
        {result && (
          <div className={styles.previews}>
            <figure>
              {/* eslint-disable-next-line @next/next/no-img-element */}
              <img className={styles.wide} src={result.url} alt="16:9 封面预览" />
              <figcaption>16:9 原图</figcaption>
            </figure>
            <figure>
              {/* eslint-disable-next-line @next/next/no-img-element */}
              <img className={styles.crop} src={result.url} alt="4:3 封面预览" />
              <figcaption>4:3 居中裁切预览</figcaption>
            </figure>
          </div>
        )}
      </div>
    </Modal>
  )
}
