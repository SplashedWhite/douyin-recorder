import { defineStore } from 'pinia'
import { ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { ElMessage } from 'element-plus'
import { DEFAULT_QUALITY } from '../constants/quality'
import type {
  LiveRoom,
  AutoMonitorMode,
  RecordTask,
  AppSettings,
  UpdateInfo,
  RecordingStatusChanged,
  RoomAutoRecordingChanged,
} from '../types'

interface RoomRefreshResult {
  total: number
  eligible: number
  succeeded: number
  failed: number
}

export const useRecorderStore = defineStore('recorder', () => {
  const rooms = ref<LiveRoom[]>([])
  const tasks = ref<RecordTask[]>([])
  const loading = ref(false)
  const isRefreshingAll = ref(false)
  const availableUpdate = ref<UpdateInfo | null>(null)
  const settings = ref<AppSettings>({
    close_behavior: 'exit',
    proxy: '',
    cookie: '',
    quality: DEFAULT_QUALITY,
    recordings_dir: '',
    db_path: '',
    auto_convert_mp4: false,
    segment_recording_enabled: false,
    segment_duration_minutes: 60,
    time_format_24h: true,
    time_display_mode: 'absolute',
    auto_check_interval_secs: 60,
    auto_monitor_window_hours: 6,
    auto_disable_after_record: true,
    notify_updates: true,
  })
  let unlistenRecordingStatus: UnlistenFn | null = null
  let unlistenSegments: UnlistenFn | null = null
  let unlistenAutoRecordingStatus: UnlistenFn | null = null
  let refreshAllPromise: Promise<RoomRefreshResult> | null = null

  function upsertTask(task: RecordTask) {
    const index = tasks.value.findIndex(item => item.id === task.id)
    if (index === -1) {
      tasks.value.unshift(task)
    } else {
      tasks.value[index] = task
    }
  }

  function upsertRoom(room: LiveRoom) {
    const index = rooms.value.findIndex(item => item.id === room.id)
    if (index === -1) {
      rooms.value.push(room)
    } else if (room.auto_record_revision >= rooms.value[index].auto_record_revision) {
      // A command response may arrive after a newer background event.
      rooms.value[index] = room
    }
  }

  async function listenRecordingEvents() {
    if (!unlistenSegments) {
      unlistenSegments = await listen<RecordTask>('recording-segments-changed', ({ payload }) => upsertTask(payload))
    }
    if (!unlistenRecordingStatus) {
      unlistenRecordingStatus = await listen<RecordingStatusChanged>('recording-status-changed', ({ payload }) => {
        upsertTask(payload.task)
        if (payload.room) upsertRoom(payload.room)
        if (!payload.message) return

        if (payload.reason === 'stream_ended' || payload.reason === 'auto_started') {
          ElMessage.success(payload.message)
        } else if (payload.reason === 'interrupted') {
          ElMessage.warning(payload.message)
        } else if (payload.reason === 'failed') {
          ElMessage.error(payload.message)
        } else if (payload.reason === 'manual_stop' && payload.message.includes('失败')) {
          ElMessage.warning(payload.message)
        }
      })
    }
    if (!unlistenAutoRecordingStatus) {
      unlistenAutoRecordingStatus = await listen<RoomAutoRecordingChanged>('room-auto-recording-changed', ({ payload }) => {
        upsertRoom(payload.room)
        if (!payload.message) return

        if (['enabled', 'scheduled', 'schedule_triggered', 'configured'].includes(payload.reason)) {
          ElMessage.success(payload.message)
        } else if (payload.reason === 'paused' || payload.reason === 'window_expired' || payload.reason === 'backoff') {
          ElMessage.warning(payload.message)
        } else {
          ElMessage.info(payload.message)
        }
      })
    }
  }

  function stopListeningRecordingEvents() {
    unlistenSegments?.()
    unlistenSegments = null
    unlistenRecordingStatus?.()
    unlistenRecordingStatus = null
    unlistenAutoRecordingStatus?.()
    unlistenAutoRecordingStatus = null
  }

  async function loadRooms() {
    try {
      const result = await invoke<LiveRoom[]>('get_rooms')
      const ids = new Set((result || []).map(room => room.id))
      rooms.value = rooms.value.filter(room => ids.has(room.id))
      for (const room of result || []) upsertRoom(room)
    } catch (e) {
      console.error('加载房间失败:', e)
    }
  }

  async function addRoom(url: string) {
    loading.value = true
    try {
      console.log('调用 add_room, url:', url)
      const room = await invoke<LiveRoom>('add_room', { url })
      console.log('返回结果:', room)
      upsertRoom(room)
    } catch (e) {
      console.error('添加房间失败:', e)
      throw e
    } finally {
      loading.value = false
    }
  }

  async function refreshRoom(roomId: number): Promise<LiveRoom> {
    try {
      const updated = await invoke<LiveRoom>('refresh_room', { roomId })
      upsertRoom(updated)
      return updated
    } catch (e) {
      console.error('刷新房间失败:', e)
      throw e
    }
  }

  function refreshAllRooms(): Promise<RoomRefreshResult> {
    if (refreshAllPromise) return refreshAllPromise

    const total = rooms.value.length
    const roomIds = rooms.value
      .filter(room => !room.auto_record_enabled)
      .map(room => room.id)
    const eligible = roomIds.length
    if (eligible === 0) {
      return Promise.resolve({ total, eligible, succeeded: 0, failed: 0 })
    }

    isRefreshingAll.value = true
    refreshAllPromise = Promise.allSettled(roomIds.map(roomId => refreshRoom(roomId)))
      .then(results => {
        const failed = results.filter(result => result.status === 'rejected').length
        if (failed > 0) {
          console.warn(`批量刷新完成，${failed} 个房间刷新失败`)
        }
        return { total, eligible, succeeded: eligible - failed, failed }
      })
      .finally(() => {
        refreshAllPromise = null
        isRefreshingAll.value = false
      })
    return refreshAllPromise
  }

  async function setRoomAutoRecord(roomId: number, enabled: boolean): Promise<LiveRoom> {
    const room = await invoke<LiveRoom>('set_room_auto_record', { roomId, enabled })
    upsertRoom(room)
    return room
  }

  async function setRoomAutoSchedule(roomId: number, dailyTime: string | null): Promise<LiveRoom> {
    const room = await invoke<LiveRoom>('set_room_auto_schedule', { roomId, dailyTime })
    upsertRoom(room)
    return room
  }

  async function setRoomAutoConfig(roomId: number, monitorMode: AutoMonitorMode, dailyTime: string | null): Promise<LiveRoom> {
    const room = await invoke<LiveRoom>('set_room_auto_config', { roomId, monitorMode, dailyTime })
    upsertRoom(room)
    return room
  }

  async function deleteRoom(id: number, cascade = false) {
    try {
      await invoke('delete_room', { id, cascade })
      rooms.value = rooms.value.filter(r => r.id !== id)
      if (cascade) {
        tasks.value = tasks.value.filter(t => t.room_id !== id)
      }
    } catch (e) {
      console.error('删除房间失败:', e)
      throw e
    }
  }

  async function getRoomTaskCount(roomId: number): Promise<number> {
    try {
      return await invoke<number>('get_room_task_count', { roomId })
    } catch (e) {
      console.error('获取任务数失败:', e)
      return 0
    }
  }

  async function loadTasks() {
    try {
      const result = await invoke<RecordTask[]>('get_tasks')
      tasks.value = result || []
    } catch (e) {
      console.error('加载任务失败:', e)
    }
  }

  async function startRecord(roomId: number) {
    try {
      const task = await invoke<RecordTask>('start_record', { roomId })
      upsertTask(task)
    } catch (e) {
      console.error('开始录制失败:', e)
      await loadTasks()
      throw e
    }
  }

  async function stopRecord(taskId: number) {
    try {
      const updated = await invoke<RecordTask>('stop_record', { taskId })
      upsertTask(updated)
      return updated
    } catch (e) {
      console.error('停止录制失败:', e)
      throw e
    }
  }

  async function deleteTask(id: number) {
    try {
      await invoke('delete_task', { id })
      tasks.value = tasks.value.filter(t => t.id !== id)
    } catch (e) {
      console.error('删除任务失败:', e)
      throw e
    }
  }

  async function convertToMp4(taskId: number): Promise<string> {
    try {
      const mp4Path = await invoke<string>('convert_to_mp4', { taskId })
      await loadTasks()
      return mp4Path
    } catch (e) {
      console.error('转换 MP4 失败:', e)
      throw e
    }
  }

  async function loadSettings() {
    try {
      const result = await invoke<AppSettings>('get_settings_cmd')
      settings.value = result
    } catch (e) {
      console.error('加载设置失败:', e)
    }
  }

  async function convertSegmentToMp4(segmentId: number): Promise<string> {
    return invoke<string>('convert_segment_to_mp4', { segmentId })
  }

  async function deleteSegment(segmentId: number) {
    await invoke('delete_segment', { segmentId })
  }

  async function saveSettings(newSettings: AppSettings) {
    try {
      const updateNotificationsWereEnabled = settings.value.notify_updates
      const savedSettings = await invoke<AppSettings>('save_settings_cmd', { newSettings })
      settings.value = savedSettings
      if (!savedSettings.notify_updates) {
        availableUpdate.value = null
      } else if (!updateNotificationsWereEnabled) {
        void checkForUpdate()
      }
    } catch (e) {
      console.error('保存设置失败:', e)
      throw e
    }
  }

  async function checkForUpdate() {
    if (!settings.value.notify_updates) {
      availableUpdate.value = null
      return
    }

    try {
      const update = await invoke<UpdateInfo | null>('check_for_update')
      availableUpdate.value = settings.value.notify_updates ? update : null
    } catch (e) {
      availableUpdate.value = null
      console.debug('检查更新失败，已静默忽略:', e)
    }
  }

  async function migrateDb(newPath: string): Promise<string> {
    try {
      const result = await invoke<string>('migrate_db_cmd', { newPath })
      settings.value.db_path = result
      return result
    } catch (e) {
      console.error('迁移数据库失败:', e)
      throw e
    }
  }

  return {
    rooms, tasks, loading, isRefreshingAll, settings, availableUpdate,
    listenRecordingEvents, stopListeningRecordingEvents,
    loadRooms, addRoom, refreshRoom, refreshAllRooms, setRoomAutoRecord, setRoomAutoSchedule, setRoomAutoConfig,
    deleteRoom, getRoomTaskCount,
    loadTasks, startRecord, stopRecord, deleteTask, convertToMp4, convertSegmentToMp4, deleteSegment,
    loadSettings, saveSettings, checkForUpdate, migrateDb
  }
})
