import type { RecordingRecovery } from '../types'

export function newerRecovery(current: RecordingRecovery | undefined, incoming: RecordingRecovery): boolean {
  return !current || incoming.recovery_id > current.recovery_id ||
    (incoming.recovery_id === current.recovery_id && incoming.revision > current.revision)
}

export function recoveryPending(status: RecordingRecovery | undefined): boolean {
  return !!status && ['confirming', 'waiting', 'starting'].includes(status.phase)
}

export function recoveryText(status: RecordingRecovery | undefined, now: number): string {
  if (!status) return ''
  if (status.phase === 'confirming') return '正在确认直播状态'
  if (status.phase === 'starting') return '正在重新录制'
  if (status.phase === 'waiting') {
    const seconds = Math.max(0, Math.ceil((Date.parse(status.next_attempt_at || status.deadline) - now) / 1000))
    return `等待重试 · ${seconds} 秒后再次确认 · 已尝试 ${status.attempts} 次`
  }
  return ''
}
