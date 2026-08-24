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
}

export interface AppSettings {
  proxy: string
  cookie: string
  quality: string
  recordings_dir: string
  db_path: string
  auto_convert_mp4: boolean
  time_format_24h: boolean
  time_display_mode: string
  auto_check_interval_secs: number
  auto_monitor_window_hours: number
  auto_disable_after_record: boolean
  notify_updates: boolean
}

export interface UpdateInfo {
  current_version: string
  latest_version: string
  release_url: string
}

export interface RecordingStatusChanged {
  task: RecordTask
  room: LiveRoom | null
  reason: 'finalizing' | 'manual_stop' | 'stream_ended' | 'interrupted' | 'failed' | 'auto_started'
  message: string | null
}

export interface RoomAutoRecordingChanged {
  room: LiveRoom
  reason: 'enabled' | 'disabled' | 'scheduled' | 'schedule_cancelled' | 'schedule_triggered' | 'window_expired' | 'paused' | 'backoff'
  message: string | null
}
