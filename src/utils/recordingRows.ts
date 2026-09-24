import type { RecordTask, RecordSegment } from '../types'

export interface TaskRow {
  id: number
  rowKey: string
  segmentId?: number
  segmentIndex?: number
  status: RecordTask['status']
  start_time: string
  file_path: string | null
  trigger: RecordTask['trigger']
  conversionState?: RecordSegment['conversion_state']
  conversionError?: string | null
}

export function recordingRows(sessions: RecordTask[]): TaskRow[] {
  return sessions.flatMap<TaskRow>(task => {
    if (!task.segment_output || !task.segments?.length) {
      return [{ ...task, rowKey: `task-${task.id}` }]
    }
    return task.segments.filter(segment => !segment.deleted).map(segment => ({
      id: task.id,
      rowKey: `segment-${segment.id}`,
      segmentId: segment.id,
      segmentIndex: segment.segment_index,
      status: (['queued', 'converting'].includes(segment.conversion_state) ||
        (task.status === 'finalizing' && segment.status === 'recording')) ? 'finalizing' as const : segment.status,
      start_time: segment.start_time,
      file_path: segment.file_path,
      trigger: task.trigger,
      conversionState: segment.conversion_state,
      conversionError: segment.conversion_error,
    }))
  }).sort((a, b) => b.start_time.localeCompare(a.start_time) || b.id - a.id || (b.segmentIndex ?? 0) - (a.segmentIndex ?? 0))
}
