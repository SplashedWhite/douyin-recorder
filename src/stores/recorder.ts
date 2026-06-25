import { defineStore } from 'pinia'
import { ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import type { LiveRoom, RecordTask, AppSettings } from '../types'

export const useRecorderStore = defineStore('recorder', () => {
  const rooms = ref<LiveRoom[]>([])
  const tasks = ref<RecordTask[]>([])
  const loading = ref(false)
  const settings = ref<AppSettings>({ proxy: '', cookie: '', quality: 'HD1', recordings_dir: '', db_path: '', auto_convert_mp4: false, time_format_24h: true, time_display_mode: 'absolute' })

  async function loadRooms() {
    try {
      const result = await invoke<LiveRoom[]>('get_rooms')
      rooms.value = result || []
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
      rooms.value.push(room)
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
      const idx = rooms.value.findIndex(r => r.id === roomId)
      if (idx !== -1) rooms.value[idx] = updated
      return updated
    } catch (e) {
      console.error('刷新房间失败:', e)
      throw e
    }
  }

  async function refreshAllRooms() {
    const results = await Promise.allSettled(
      rooms.value.map(room => refreshRoom(room.id))
    )
    const failed = results.filter(r => r.status === 'rejected').length
    if (failed > 0) {
      console.warn(`批量刷新完成，${failed} 个房间刷新失败`)
    }
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
      tasks.value.unshift(task)
    } catch (e) {
      console.error('开始录制失败:', e)
      throw e
    }
  }

  async function stopRecord(taskId: number) {
    try {
      const updated = await invoke<RecordTask>('stop_record', { taskId })
      const idx = tasks.value.findIndex(t => t.id === taskId)
      if (idx !== -1) tasks.value[idx] = updated
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
      const task = tasks.value.find(t => t.id === taskId)
      if (task) task.file_path = mp4Path
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

  async function saveSettings(newSettings: AppSettings) {
    try {
      await invoke('save_settings_cmd', { newSettings })
      settings.value = newSettings
    } catch (e) {
      console.error('保存设置失败:', e)
      throw e
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
    rooms, tasks, loading, settings,
    loadRooms, addRoom, refreshRoom, refreshAllRooms, deleteRoom, getRoomTaskCount,
    loadTasks, startRecord, stopRecord, deleteTask, convertToMp4,
    loadSettings, saveSettings, migrateDb
  }
})