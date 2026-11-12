import { API_BASE } from '../../lib/api-streamer'

export type DraftSource =
  | { kind: 'recording'; name: string }
  | { kind: 'join'; id: string }
  | { kind: 'hardsubs'; id: string }
  | { kind: 'local'; file: File }

export function draftMedia(source: DraftSource, localUrl: string) {
  if (source.kind === 'join' || source.kind === 'hardsubs') {
    const output = API_BASE + '/v1/clip-library/output/' + encodeURIComponent(source.id)
    return { url: source.kind === 'join' ? output : output + '/hardsubs?inline=true' }
  }
  const name = source.kind === 'recording' ? source.name : source.file.name
  const extension = name.toLowerCase().split('.').pop()
  const type = extension === 'flv' ? 'flv' as const : extension === 'ts' ? 'mpegts' as const : undefined
  const url = source.kind === 'recording' ? API_BASE + '/static/' + encodeURIComponent(name) : localUrl
  return { url, type }
}
