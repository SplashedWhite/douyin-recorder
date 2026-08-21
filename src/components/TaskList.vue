<template>
  <el-card class="task-list">
    <template #header>
      <div class="card-header">
        <div class="card-title">
          <span class="title-text">录制任务</span>
          <span class="title-count" v-if="tasks.length">{{ tasks.length }}</span>
        </div>
      </div>
    </template>

    <!-- Empty State -->
    <div v-if="tasks.length === 0" class="empty-state">
      <div class="empty-icon">
        <svg viewBox="0 0 48 48" fill="none">
          <path d="M28 8h-8L16 14H8a4 4 0 00-4 4v18a4 4 0 004 4h32a4 4 0 004-4V18a4 4 0 00-4-4h-8l-4-6z" stroke="currentColor" stroke-width="2" />
          <circle cx="24" cy="28" r="6" stroke="currentColor" stroke-width="2" />
          <path d="M24 24v4l3 2" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
        </svg>
      </div>
      <p class="empty-title">暂无录制任务</p>
      <p class="empty-desc">在房间列表中点击录制按钮开始</p>
    </div>

    <!-- Task Cards -->
    <div v-else class="task-cards">
      <TransitionGroup name="task">
        <div
          v-for="task in tasks"
          :key="task.id"
          class="task-card"
          :class="task.status"
        >
          <!-- Left accent stripe -->
          <div class="task-accent" :class="task.status"></div>

          <div class="task-main">
            <div class="task-header-row">
              <span class="task-status-badge" :class="task.status">
                <span class="badge-dot"></span>
                {{ getStatusText(task.status) }}
              </span>
              <span v-if="task.trigger === 'auto'" class="task-trigger-badge">自动</span>
              <span class="task-time">{{ formatTime(task.start_time) }}</span>
            </div>
            <div class="task-path" v-if="task.file_path">
              {{ task.file_path.split(/[/\\]/).pop() }}
            </div>
          </div>

          <div class="task-actions">
            <button
              v-if="task.status === 'recording'"
              class="icon-btn warning"
              @click="stopRecord(task)"
              title="停止录制"
            >
              <el-icon :size="15"><VideoPause /></el-icon>
            </button>
            <button
              v-if="canAccessFile(task)"
              class="icon-btn primary"
              @click="openFile(task)"
              title="打开文件"
            >
              <el-icon :size="15"><FolderOpened /></el-icon>
            </button>
            <button
              v-if="canAccessFile(task)"
              class="icon-btn primary"
              @click="openFolder(task)"
              title="打开文件夹"
            >
              <el-icon :size="15"><Folder /></el-icon>
            </button>
            <button
              v-if="canConvert(task)"
              class="icon-btn primary"
              @click="convertToMp4(task)"
              title="转换为 MP4"
            >
              <el-icon :size="15"><Switch /></el-icon>
            </button>
            <button
              class="icon-btn danger"
              @click="deleteTask(task)"
              :disabled="task.status === 'recording' || task.status === 'finalizing'"
              title="删除任务"
            >
              <el-icon :size="15"><Delete /></el-icon>
            </button>
          </div>
        </div>
      </TransitionGroup>
    </div>
  </el-card>
</template>

<script setup lang="ts">
import { storeToRefs } from 'pinia'
import { useRecorderStore } from '../stores/recorder'
import { ElMessage } from 'element-plus'
import { Delete, VideoPause, FolderOpened, Folder, Switch } from '@element-plus/icons-vue'
import { openPath, revealItemInDir } from '@tauri-apps/plugin-opener'
import type { RecordTask } from '../types'

const store = useRecorderStore()
const { tasks, settings } = storeToRefs(store)

function getStatusText(status: string) {
  const map: Record<string, string> = {
    waiting: '等待中',
    recording: '录制中',
    finalizing: '结束处理中',
    completed: '已完成',
    interrupted: '录制中断',
    failed: '失败',
  }
  return map[status] || status
}

function formatTime(time: string) {
  if (!time) return ''
  try {
    // SQLite datetime('now') is UTC; append 'Z' to parse correctly
    const date = new Date(time.includes('Z') || time.includes('+') ? time : time + 'Z')
    if (isNaN(date.getTime())) return time

    const now = new Date()

    // Convert to Beijing time (UTC+8) for display
    const bjOffset = 8 * 60 * 60 * 1000
    const bjDate = new Date(date.getTime() + bjOffset)
    const bjNow = new Date(now.getTime() + bjOffset)

    const y = bjDate.getUTCFullYear()
    const mo = bjDate.getUTCMonth()
    const d = bjDate.getUTCDate()
    const h = bjDate.getUTCHours()
    const min = bjDate.getUTCMinutes()

    const ny = bjNow.getUTCFullYear()
    const nmo = bjNow.getUTCMonth()
    const nd = bjNow.getUTCDate()

    const is24h = settings.value.time_format_24h
    const mode = settings.value.time_display_mode

    // Format time portion
    function fmtTime(h: number, m: number): string {
      if (is24h) {
        return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`
      }
      const period = h < 12 ? 'AM' : 'PM'
      const h12 = h === 0 ? 12 : h > 12 ? h - 12 : h
      return `${h12}:${String(m).padStart(2, '0')} ${period}`
    }

    const timeStr = fmtTime(h, min)

    // Relative mode
    if (mode === 'relative') {
      const diffMs = now.getTime() - date.getTime()
      if (diffMs < 0) return timeStr

      const diffSec = Math.floor(diffMs / 1000)
      const diffMin = Math.floor(diffSec / 60)
      const diffHour = Math.floor(diffMin / 60)
      const diffDay = Math.floor(diffHour / 24)

      if (diffSec < 60) return '刚刚'
      if (diffMin < 60) return `${diffMin} 分钟前`
      if (diffHour < 24) return `${diffHour} 小时前`
      if (diffDay < 30) return `${diffDay} 天前`
      if (diffDay < 365) return `${Math.floor(diffDay / 30)} 个月前`
      return `${Math.floor(diffDay / 365)} 年前`
    }

    // Absolute mode with smart formatting
    if (y !== ny) {
      // Different year: show year/month/day time
      return `${y}/${String(mo + 1).padStart(2, '0')}/${String(d).padStart(2, '0')} ${timeStr}`
    }
    if (mo !== nmo) {
      // Different month: show month/day time
      return `${mo + 1}/${String(d).padStart(2, '0')} ${timeStr}`
    }
    if (d !== nd) {
      // Different day: show day time
      return `${d}日 ${timeStr}`
    }
    // Same day: show time only
    return timeStr
  } catch {
    return time
  }
}

async function stopRecord(task: RecordTask) {
  try {
    await store.stopRecord(task.id)
    ElMessage.success('录制已停止')
  } catch (e) {
    ElMessage.error(`停止失败: ${e}`)
  }
}

async function openFile(task: RecordTask) {
  if (!task.file_path) {
    ElMessage.warning('文件路径为空')
    return
  }
  try {
    await openPath(task.file_path)
  } catch (e) {
    ElMessage.error(`打开文件失败: ${e}`)
  }
}

async function openFolder(task: RecordTask) {
  if (!task.file_path) {
    ElMessage.warning('文件路径为空')
    return
  }
  try {
    await revealItemInDir(task.file_path)
  } catch (e) {
    ElMessage.error(`打开文件夹失败: ${e}`)
  }
}

function isFlvFile(task: RecordTask): boolean {
  return !!task.file_path && task.file_path.endsWith('.flv')
}

function canAccessFile(task: RecordTask): boolean {
  return !!task.file_path && ['completed', 'interrupted', 'failed'].includes(task.status)
}

function canConvert(task: RecordTask): boolean {
  return canAccessFile(task) && isFlvFile(task)
}

async function convertToMp4(task: RecordTask) {
  try {
    await store.convertToMp4(task.id)
    ElMessage.success('已转换为 MP4')
  } catch (e) {
    ElMessage.error(`转换失败: ${e}`)
  }
}

async function deleteTask(task: RecordTask) {
  try {
    await store.deleteTask(task.id)
    ElMessage.success('删除成功')
  } catch (e) {
    ElMessage.error(`删除失败: ${e}`)
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

/* ── Task Cards ── */
.task-cards {
  padding: 4px 0;
}

.task-card {
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 12px 20px 12px 0;
  transition: background var(--transition-fast);
  position: relative;
}

.task-card:hover {
  background: var(--color-surface-hover);
}

/* ── Left Accent Stripe ── */
.task-accent {
  width: 3px;
  align-self: stretch;
  border-radius: 0 2px 2px 0;
  flex-shrink: 0;
  opacity: 0;
  transition: opacity var(--transition-fast);
}

.task-card.recording .task-accent {
  background: var(--color-warning);
  opacity: 1;
}

.task-card.completed .task-accent {
  background: var(--color-success);
  opacity: 0.5;
}

.task-card.finalizing .task-accent {
  background: var(--color-primary);
  opacity: 0.5;
}

.task-card.interrupted .task-accent {
  background: var(--color-warning);
  opacity: 0.7;
}

.task-card.failed .task-accent {
  background: var(--color-danger);
  opacity: 0.6;
}

/* ── Task Main ── */
.task-main {
  flex: 1;
  min-width: 0;
  padding-left: 20px;
}

.task-header-row {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 3px;
}

.task-status-badge {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  font-size: 11px;
  font-weight: 600;
  padding: 2px 8px;
  border-radius: var(--radius-full);
  background: var(--color-bg);
  color: var(--color-text-tertiary);
  letter-spacing: 0.2px;
}

.task-status-badge.recording {
  background: var(--color-warning-light);
  color: var(--color-warning);
}

.task-status-badge.completed {
  background: var(--color-success-light);
  color: var(--color-success);
}

.task-status-badge.finalizing {
  background: var(--color-primary-light);
  color: var(--color-primary);
}

.task-status-badge.interrupted {
  background: var(--color-warning-light);
  color: var(--color-warning);
}

.task-status-badge.failed {
  background: var(--color-danger-light);
  color: var(--color-danger);
}

.task-status-badge.waiting {
  background: var(--color-primary-light);
  color: var(--color-primary);
}

.badge-dot {
  width: 5px;
  height: 5px;
  border-radius: var(--radius-full);
  background: currentColor;
}

.task-status-badge.recording .badge-dot {
  animation: pulse 1.5s ease-in-out infinite;
}

@keyframes pulse {
  0%, 100% { opacity: 1; transform: scale(1); }
  50% { opacity: 0.4; transform: scale(0.85); }
}

.task-time {
  font-size: 11px;
  color: var(--color-text-tertiary);
  font-variant-numeric: tabular-nums;
}

.task-trigger-badge {
  font-size: 10px;
  font-weight: 600;
  padding: 2px 6px;
  border-radius: var(--radius-full);
  color: var(--color-primary);
  background: var(--color-primary-light);
}

.task-path {
  font-size: 12px;
  color: var(--color-text-secondary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  font-family: 'SF Mono', 'Menlo', 'Consolas', monospace;
  font-size: 11px;
}

/* ── Actions ── */
.task-actions {
  display: flex;
  align-items: center;
  gap: 2px;
  flex-shrink: 0;
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

.icon-btn.primary:hover {
  background: var(--color-primary-light);
  color: var(--color-primary);
}

.icon-btn.warning:hover {
  background: var(--color-warning-light);
  color: var(--color-warning);
}

.icon-btn.danger:hover {
  background: var(--color-danger-light);
  color: var(--color-danger);
}

.icon-btn:disabled {
  opacity: 0.35;
  cursor: not-allowed;
  pointer-events: none;
}

/* ── Transition ── */
.task-enter-active {
  transition: all 0.3s ease;
}

.task-leave-active {
  transition: all 0.2s ease;
}

.task-enter-from {
  opacity: 0;
  transform: translateY(-8px);
}

.task-leave-to {
  opacity: 0;
  transform: translateX(-10px);
}
</style>
