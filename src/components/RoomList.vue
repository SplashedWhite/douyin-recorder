<template>
  <el-card class="room-list">
    <template #header>
      <div class="card-header">
        <div class="card-title">
          <span class="title-text">监控房间</span>
          <span class="title-count" v-if="rooms.length">{{ rooms.length }}</span>
        </div>
      </div>
    </template>

    <!-- Inline Add Bar -->
    <div class="add-bar">
      <div class="add-input-wrap">
        <el-icon class="add-icon"><Plus /></el-icon>
        <input
          v-model="newUrl"
          class="add-input"
          placeholder="粘贴抖音直播间链接 (live.douyin.com/...)"
          @keyup.enter="handleAdd"
        />
      </div>
      <el-button
        v-if="newUrl.trim()"
        type="primary"
        size="small"
        class="add-btn"
        :loading="loading"
        @click="handleAdd"
      >
        添加
      </el-button>
    </div>

    <!-- Empty State -->
    <div v-if="rooms.length === 0 && !newUrl.trim()" class="empty-state">
      <div class="empty-icon">
        <svg viewBox="0 0 48 48" fill="none">
          <rect x="4" y="8" width="40" height="28" rx="4" stroke="currentColor" stroke-width="2" />
          <path d="M20 36h8" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
          <path d="M24 36v4" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
          <circle cx="24" cy="22" r="4" stroke="currentColor" stroke-width="2" />
          <path d="M18 18l2 2m8-2l-2 2" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
        </svg>
      </div>
      <p class="empty-title">暂无监控房间</p>
      <p class="empty-desc">在上方输入框粘贴直播间链接开始使用</p>
    </div>

    <!-- Room Cards -->
    <div v-else class="room-cards">
      <TransitionGroup name="room">
        <div v-for="room in rooms" :key="room.id" class="room-card">
          <!-- Avatar -->
          <div class="room-avatar" :class="{ live: room.is_live }">
            <img
              v-if="room.avatar_url"
              :src="room.avatar_url"
              :alt="room.anchor_name"
              class="avatar-img"
              @error="($event.target as HTMLImageElement).style.display = 'none'"
            />
            <span v-else class="avatar-fallback">{{ getInitial(room.anchor_name) }}</span>
            <span v-if="room.is_live" class="live-dot"></span>
          </div>

          <!-- Info -->
          <div class="room-info">
            <div class="room-name">{{ room.anchor_name || '未知主播' }}</div>
            <div class="room-title">{{ room.room_title || '暂无标题' }}</div>
            <div class="room-status" :class="{ live: room.is_live }">
              <span class="status-dot"></span>
              {{ room.is_live ? '直播中' : '未开播' }}
            </div>
          </div>

          <!-- Actions -->
          <div class="room-actions">
            <el-button
              size="small"
              :type="isRecording(room.id) ? 'warning' : 'primary'"
              @click="toggleRecord(room)"
              class="record-btn"
            >
              <el-icon v-if="isRecording(room.id)" class="btn-icon"><VideoPause /></el-icon>
              <el-icon v-else class="btn-icon"><VideoPlay /></el-icon>
              {{ isRecording(room.id) ? '停止' : '录制' }}
            </el-button>
            <button
              class="icon-btn"
              @click="refreshRoom(room)"
              :disabled="refreshingIds.has(room.id)"
              title="刷新状态"
            >
              <el-icon :size="15" :class="{ 'is-loading': refreshingIds.has(room.id) }"><Refresh /></el-icon>
            </button>
            <button class="icon-btn danger" @click="deleteRoom(room)" title="删除">
              <el-icon :size="15"><Delete /></el-icon>
            </button>
          </div>
        </div>
      </TransitionGroup>
    </div>
  </el-card>
</template>

<script setup lang="ts">
import { ref } from 'vue'
import { storeToRefs } from 'pinia'
import { Plus, Delete, VideoPlay, VideoPause, Refresh } from '@element-plus/icons-vue'
import { useRecorderStore } from '../stores/recorder'
import { ElMessage, ElMessageBox } from 'element-plus'
import type { LiveRoom } from '../types'

const store = useRecorderStore()
const { rooms, tasks, loading } = storeToRefs(store)

const newUrl = ref('')
const refreshingIds = ref(new Set<number>())

function getInitial(name: string) {
  return (name || '?').charAt(0).toUpperCase()
}

function getActiveTask(roomId: number) {
  return tasks.value.find(t => t.room_id === roomId && t.status === 'recording')
}

function isRecording(roomId: number) {
  return !!getActiveTask(roomId)
}

async function toggleRecord(room: LiveRoom) {
  const activeTask = getActiveTask(room.id)
  if (activeTask) {
    try {
      await store.stopRecord(activeTask.id)
      ElMessage.success('录制已停止')
    } catch (e) {
      ElMessage.error(`停止失败: ${e}`)
    }
  } else {
    try {
      await store.startRecord(room.id)
      ElMessage.success('开始录制')
    } catch (e) {
      ElMessage.error(`录制失败: ${e}`)
    }
  }
}

async function refreshRoom(room: LiveRoom) {
  refreshingIds.value.add(room.id)
  try {
    await store.refreshRoom(room.id)
    ElMessage.success('状态已刷新')
  } catch (e) {
    ElMessage.error(`刷新失败: ${e}`)
  } finally {
    refreshingIds.value.delete(room.id)
  }
}

async function handleAdd() {
  if (!newUrl.value.trim()) return
  try {
    await store.addRoom(newUrl.value)
    newUrl.value = ''
    ElMessage.success('添加成功')
  } catch (e) {
    ElMessage.error(`添加失败: ${e}`)
  }
}

async function deleteRoom(room: LiveRoom) {
  try {
    await store.deleteRoom(room.id)
    ElMessage.success('删除成功')
  } catch (e: any) {
    const msg = String(e)
    if (msg.includes('录制任务')) {
      try {
        await ElMessageBox.confirm(
          `该房间还有录制任务，删除房间会同时删除所有关联的任务记录。确定删除「${room.anchor_name}」吗？`,
          '确认删除',
          { confirmButtonText: '确定删除', cancelButtonText: '取消', type: 'warning' }
        )
        await store.deleteRoom(room.id, true)
        ElMessage.success('删除成功')
      } catch {
        // user cancelled
      }
    } else {
      ElMessage.error(`删除失败: ${e}`)
    }
  }
}
</script>

<style scoped>
.card-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.card-title {
  display: flex;
  align-items: center;
  gap: 8px;
}

.title-text {
  font-size: 15px;
  font-weight: 600;
  color: var(--color-text);
  letter-spacing: -0.2px;
}

.title-count {
  font-size: 11px;
  font-weight: 600;
  color: var(--color-text-tertiary);
  background: var(--color-bg);
  width: 22px;
  height: 22px;
  border-radius: var(--radius-full);
  display: inline-flex;
  align-items: center;
  justify-content: center;
}

/* ── Add Bar ── */
.add-bar {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 12px 16px;
  border-bottom: 1px solid var(--color-border-light);
}

.add-input-wrap {
  flex: 1;
  display: flex;
  align-items: center;
  gap: 8px;
  background: var(--color-bg);
  border-radius: var(--radius-sm);
  padding: 8px 12px;
  transition: all var(--transition-fast);
  border: 1.5px solid transparent;
}

.add-input-wrap:focus-within {
  background: var(--color-surface);
  border-color: var(--color-primary);
  box-shadow: 0 0 0 3px var(--color-primary-light);
}

.add-icon {
  color: var(--color-text-tertiary);
  flex-shrink: 0;
  font-size: 16px;
}

.add-input {
  flex: 1;
  border: none;
  outline: none;
  background: transparent;
  font-size: 13px;
  color: var(--color-text);
  font-family: inherit;
}

.add-input::placeholder {
  color: var(--color-text-tertiary);
}

.add-btn {
  flex-shrink: 0;
  animation: fadeIn 0.15s ease;
}

@keyframes fadeIn {
  from { opacity: 0; transform: scale(0.95); }
  to { opacity: 1; transform: scale(1); }
}

/* ── Empty State ── */
.empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 40px 24px 48px;
}

.empty-icon {
  width: 56px;
  height: 56px;
  color: var(--color-border);
  margin-bottom: 16px;
}

.empty-icon svg {
  width: 100%;
  height: 100%;
}

.empty-title {
  font-size: 15px;
  font-weight: 600;
  color: var(--color-text-secondary);
  margin-bottom: 4px;
}

.empty-desc {
  font-size: 13px;
  color: var(--color-text-tertiary);
}

/* ── Room Cards ── */
.room-cards {
  padding: 4px 0;
}

.room-card {
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 14px 20px;
  transition: background var(--transition-fast);
}

.room-card:hover {
  background: var(--color-surface-hover);
}

/* ── Avatar ── */
.room-avatar {
  width: 44px;
  height: 44px;
  border-radius: var(--radius-full);
  overflow: hidden;
  flex-shrink: 0;
  position: relative;
  background: linear-gradient(135deg, #e8e8ed, #d2d2d7);
  display: flex;
  align-items: center;
  justify-content: center;
}

.room-avatar.live {
  box-shadow: 0 0 0 2px var(--color-surface), 0 0 0 3.5px var(--color-success);
}

.avatar-img {
  width: 100%;
  height: 100%;
  object-fit: cover;
}

.avatar-fallback {
  font-size: 17px;
  font-weight: 600;
  color: var(--color-text-secondary);
  user-select: none;
}

.live-dot {
  position: absolute;
  bottom: 1px;
  right: 1px;
  width: 10px;
  height: 10px;
  background: var(--color-success);
  border: 2px solid var(--color-surface);
  border-radius: var(--radius-full);
  animation: livePulse 2s ease-in-out infinite;
}

@keyframes livePulse {
  0%, 100% { box-shadow: 0 0 0 0 rgba(52, 199, 89, 0.4); }
  50% { box-shadow: 0 0 0 4px rgba(52, 199, 89, 0); }
}

/* ── Room Info ── */
.room-info {
  flex: 1;
  min-width: 0;
}

.room-name {
  font-size: 14px;
  font-weight: 600;
  color: var(--color-text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  letter-spacing: -0.1px;
  line-height: 1.3;
}

.room-title {
  font-size: 12px;
  color: var(--color-text-secondary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  margin-top: 1px;
  line-height: 1.3;
}

.room-status {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 11px;
  font-weight: 500;
  color: var(--color-text-tertiary);
  margin-top: 4px;
}

.room-status.live {
  color: var(--color-success);
}

.status-dot {
  width: 5px;
  height: 5px;
  border-radius: var(--radius-full);
  background: currentColor;
  opacity: 0.6;
}

.room-status.live .status-dot {
  opacity: 1;
}

/* ── Actions ── */
.room-actions {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-shrink: 0;
}

.record-btn {
  min-width: 76px !important;
}

.btn-icon {
  margin-right: 3px;
}

.icon-btn {
  width: 30px;
  height: 30px;
  border: none;
  background: transparent;
  border-radius: var(--radius-sm);
  cursor: pointer;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  color: var(--color-text-tertiary);
  transition: all var(--transition-fast);
}

.icon-btn:hover {
  background: var(--color-bg);
  color: var(--color-text-secondary);
}

.icon-btn.danger:hover {
  background: var(--color-danger-light);
  color: var(--color-danger);
}

/* ── Transition ── */
.room-enter-active {
  transition: all 0.3s ease;
}

.room-leave-active {
  transition: all 0.2s ease;
}

.room-enter-from {
  opacity: 0;
  transform: translateY(-8px);
}

.room-leave-to {
  opacity: 0;
  transform: translateX(-10px);
}
</style>
