export type AutoMonitorMode = 'window' | 'continuous'

export interface LiveRoom {
  id: number
  platform: string
  room_id: string
  anchor_name: string
  room_title: string
  cover_url: string
  avatar_url: string
  is_live: boolean
  created_at: string
  auto_record_enabled: boolean
  auto_monitor_mode: AutoMonitorMode
  auto_record_retry_at: string | null
  auto_record_error: string | null
  auto_record_revision: number
  auto_record_daily_time: string | null
  auto_record_until: string | null
  last_schedule_trigger_date: string | null
}

export interface RecordTask {
  id: number
  room_id: number
  status: 'waiting' | 'recording' | 'finalizing' | 'completed' | 'interrupted' | 'failed'
  start_time: string
  end_time: string | null
  file_path: string | null
  file_size: number | null
  trigger: 'manual' | 'auto'
  recovery_from_task_id: number | null
  segment_output: { file_prefix: string; output_pattern: string; manifest_path: string; duration_secs: number } | null
  segments: RecordSegment[]
}

export interface RecordSegment {
  id: number
  task_id: number
  segment_index: number
  file_path: string
  file_size: number
  start_time: string
  end_time: string | null
  status: RecordTask['status']
  conversion_state: 'idle' | 'queued' | 'converting' | 'failed'
  conversion_error: string | null
  deleted: boolean
}

export interface AppSettings {
  close_behavior: 'exit' | 'tray'
  proxy: string
  cookie: string
  quality: string
  recordings_dir: string
  db_path: string
  auto_convert_mp4: boolean
  segment_recording_enabled: boolean
  segment_duration_minutes: number
  ffmpeg_reconnect_enabled: boolean
  recording_recovery_enabled: boolean
  recording_recovery_timeout_secs: number
  ffmpeg_rw_timeout_secs: number
  ffmpeg_reconnect_max_retries: number
  ffmpeg_reconnect_delay_max_secs: number
  ffmpeg_reconnect_delay_total_max_secs: number
  time_format_24h: boolean
  time_display_mode: string
  auto_check_interval_secs: number
  auto_monitor_window_hours: number
  auto_disable_after_record: boolean
  notify_updates: boolean
  log_max_size_mib: number
  log_backup_count: number
  api_log_enabled: boolean
  api_log_max_size_mib: number
  api_log_backup_count: number
}

export interface LifecycleStatus {
  exiting: boolean
  revision: number
  message: string | null
}

export interface RecordingRecovery {
  recovery_id: number
  room_id: number
  from_task_id: number
  media_received: boolean
  task_id: number
  trigger: 'manual' | 'auto'
  phase: 'confirming' | 'waiting' | 'starting' | 'recording' | 'stable' | 'offline' | 'exhausted' | 'failed' | 'cancelled'
  attempts: number
  next_attempt_at: string | null
  deadline: string
  last_error: string | null
  revision: number
}

export interface RecordingLogInfo {
  directory: string
  last_error: string | null
  api_last_error: string | null
}

export interface UpdateInfo {
  current_version: string
  latest_version: string
  release_url: string
}

export interface RecordingStatusChanged {
  task: RecordTask
  room: LiveRoom | null
  reason: 'finalizing' | 'manual_stop' | 'stream_ended' | 'interrupted' | 'failed' | 'auto_started' | 'conversion_started' | 'conversion_finished' | 'recovery_started'
  message: string | null
}

export interface RoomAutoRecordingChanged {
  room: LiveRoom
  reason: 'enabled' | 'disabled' | 'scheduled' | 'schedule_cancelled' | 'schedule_triggered' | 'window_expired' | 'paused' | 'backoff' | 'configured' | 'state_changed'
  message: string | null
}
