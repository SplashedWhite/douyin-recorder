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
}

export interface RecordTask {
  id: number
  room_id: number
  status: 'waiting' | 'recording' | 'completed' | 'failed'
  start_time: string
  end_time: string | null
  file_path: string | null
  file_size: number | null
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
}