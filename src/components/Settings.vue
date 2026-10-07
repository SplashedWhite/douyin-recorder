<template>
  <el-dialog v-model="visible" title="设置" width="460" :show-close="!migrating" :close-on-click-modal="!migrating" :close-on-press-escape="!migrating" @open="onOpen">
    <div class="settings-body">
      <div class="settings-section">
        <div class="section-header">
          <span class="section-label">点击关闭按钮时</span>
        </div>
        <el-select v-model="form.close_behavior" size="large" style="width: 100%">
          <el-option label="直接退出程序" value="exit" />
          <el-option label="关闭到系统托盘" value="tray" />
        </el-select>
        <div class="quality-note">
          关闭到托盘后继续录制和监控，点击托盘图标恢复窗口。直接退出会先停止录制并保存文件。
        </div>
      </div>

      <div class="settings-section">
        <div class="section-header">
          <span class="section-label">代理设置</span>
          <span class="section-hint">留空则不使用代理</span>
        </div>
        <el-input
          v-model="form.proxy"
          placeholder="http://127.0.0.1:7890"
          clearable
          size="large"
        />
      </div>

      <div class="settings-section">
        <div class="section-header">
          <span class="section-label">Cookie</span>
          <span class="section-hint">遇到反爬限制时填入浏览器 Cookie</span>
        </div>
        <el-input
          v-model="form.cookie"
          placeholder="__ac_nonce=xxx; __ac_signature=xxx"
          clearable
          size="large"
        />
      </div>

      <div class="settings-section">
        <div class="section-header">
          <span class="section-label">画质偏好</span>
        </div>
        <el-select v-model="form.quality" size="large" style="width: 100%">
          <el-option v-for="option in QUALITY_OPTIONS" :key="option.value" :label="option.label" :value="option.value" />
        </el-select>
        <div class="quality-note">
          优先录制所选画质，不可用时自动选择其他档位。实际清晰度由直播间提供。
        </div>
      </div>

      <div class="settings-section">
        <div class="section-header">
          <span class="section-label">自动录制</span>
          <span class="section-hint">只对房间中已开启自动录制的项目生效</span>
        </div>
        <div class="auto-settings-panel">
          <div class="auto-setting-row">
            <div>
              <div class="auto-setting-label">开播检测间隔</div>
              <div class="auto-setting-hint">建议保持 60 秒或更长，降低请求频率</div>
            </div>
            <div class="number-setting">
              <el-input-number
                v-model="form.auto_check_interval_secs"
                :min="10"
                :max="3600"
                :step="10"
                controls-position="right"
              />
              <span>秒</span>
            </div>
          </div>
          <div class="auto-setting-row">
            <div>
              <div class="auto-setting-label">单次监控窗口</div>
              <div class="auto-setting-hint">仅限时监控：到期仍未开播时自动停止请求</div>
            </div>
            <div class="number-setting">
              <el-input-number
                v-model="form.auto_monitor_window_hours"
                :min="1"
                :max="24"
                :step="1"
                controls-position="right"
              />
              <span>小时</span>
            </div>
          </div>
          <div class="auto-setting-row">
            <div>
              <div class="auto-setting-label">自动录完一场后</div>
              <div class="auto-setting-hint">仅限时监控：影响由自动检测启动的录制</div>
            </div>
            <el-select v-model="form.auto_disable_after_record" style="width: 164px; flex-shrink: 0">
              <el-option label="关闭自动录制" :value="true" />
              <el-option label="重新开始一个窗口" :value="false" />
            </el-select>
          </div>
        </div>
        <div class="auto-settings-note">
          各房间可在“录制设置”中选择监控方式。持续监控不受窗口时长和录后选项影响；录制期间不会轮询。
        </div>
      </div>

      <div class="settings-section">
        <div class="section-header">
          <span class="section-label">录制保存目录</span>
          <span class="section-hint">留空使用默认目录</span>
        </div>
        <el-input
          v-model="form.recordings_dir"
          placeholder="D:\Recordings"
          clearable
          size="large"
        />
      </div>

      <div class="settings-section">
        <div class="section-header">
          <span class="section-label">自动转换 MP4</span>
          <span class="section-hint">每段录完后转换；未分段时在整场结束后转换</span>
        </div>
        <el-switch
          v-model="form.auto_convert_mp4"
          active-text="开启"
          inactive-text="关闭"
        />
      </div>

      <div class="settings-section">
        <div class="section-header">
          <span class="section-label">分段录制</span>
          <span class="section-hint">全局生效，适用于所有新开始的录制</span>
        </div>
        <el-switch v-model="form.segment_recording_enabled" active-text="开启" inactive-text="关闭" />
        <div v-if="form.segment_recording_enabled" class="auto-setting-row">
          <span class="auto-setting-label">每段时长</span>
          <div class="number-setting">
            <el-input-number v-model="form.segment_duration_minutes" :min="1" :max="4294967295" :precision="0" :step="1" controls-position="right" />
            <span>分钟</span>
          </div>
        </div>
        <div class="quality-note">每段约为指定时长，文件名自动添加 part001 等序号，每段单独显示一条记录。修改后对新开始的录制生效。</div>
      </div>

      <div class="settings-section">
        <div class="section-header">
          <span class="section-label">时间格式</span>
          <span class="section-hint">任务列表中的时间显示方式</span>
        </div>
        <div class="time-options">
          <el-select v-model="form.time_display_mode" size="large" style="flex: 1">
            <el-option label="显示实际日期" value="absolute" />
            <el-option label="显示距离现在" value="relative" />
          </el-select>
          <el-select v-model="form.time_format_24h" size="large" style="width: 120px">
            <el-option label="24 小时制" :value="true" />
            <el-option label="12 小时制" :value="false" />
          </el-select>
        </div>
      </div>

      <div class="settings-section">
        <div class="section-header">
          <span class="section-label">更新提醒</span>
          <span class="section-hint">启动时静默检查 GitHub 最新正式版本</span>
        </div>
        <el-switch
          v-model="form.notify_updates"
          active-text="开启"
          inactive-text="关闭"
        />
        <div class="update-settings-note">
          关闭后不会请求 GitHub，也不会显示更新提示。
        </div>
      </div>

      <div class="settings-section">
        <div class="section-header">
          <span class="section-label">诊断日志</span>
          <span class="section-hint">自动保存，方便排查录制问题</span>
        </div>
        <div class="auto-settings-panel log-retention">
          <div class="auto-setting-row">
            <span class="auto-setting-label">单个日志大小</span>
            <div class="number-setting">
              <el-input-number v-model="form.log_max_size_mib" aria-label="单个日志大小" :min="1" :max="1024" :precision="0" :step="1" controls-position="right" />
              <span>MiB</span>
            </div>
          </div>
          <div class="auto-setting-row">
            <div>
              <div class="auto-setting-label">历史日志保留份数</div>
              <div class="auto-setting-hint">不含当前正在写入的文件</div>
            </div>
            <div class="number-setting">
              <el-input-number v-model="form.log_backup_count" aria-label="历史日志保留份数" :min="1" :max="100" :precision="0" :step="1" controls-position="right" />
              <span>份</span>
            </div>
          </div>
        </div>
        <div class="quality-note log-budget">预计总占用约 {{ logBudgetText }}，默认约 25 MiB。保存后生效；调小份数会清理更旧的日志，已有大文件随轮换逐步替换。</div>
        <div v-if="logInfo" class="log-directory">{{ logInfo.directory }}</div>
        <el-button @click="openLogFolder" :loading="openingLogs" :disabled="loadingLogs">打开日志文件夹</el-button>
        <div class="quality-note">包含录制和自动监控记录。遇到问题时，可将文件夹中的日志文件提供给开发者。</div>
        <div v-if="logInfo?.last_error" class="log-error" role="alert">最近一次保存失败（部分日志可能缺失）：{{ logInfo.last_error }}</div>
        <div v-if="logInfoError" class="log-error" role="alert">{{ logInfoError }}</div>
      </div>

      <div class="settings-section">
        <div class="section-header">
          <span class="section-label">数据库位置</span>
          <span class="section-hint">迁移完成后立即生效，原数据库保留</span>
        </div>
        <div class="db-row">
          <el-input
            v-model="migrationTarget"
            placeholder="D:\Data\douyin_recorder.db"
            :disabled="migrating || saving"
            clearable
            size="large"
          />
          <el-button size="large" @click="onMigrate" :loading="migrating" :disabled="!migrationTarget.trim() || saving || hasRunningTasks">
            迁移
          </el-button>
        </div>
        <div class="db-current" v-if="store.settings.db_path">
          当前: {{ store.settings.db_path }}
        </div>
        <div class="db-current hint-warn" v-if="hasRunningTasks">
          有任务正在录制或结束处理中，请等待结束后再迁移。
        </div>
      </div>
    </div>

    <template #footer>
      <div class="footer-row">
        <span class="version-text">v{{ version }}</span>
        <div>
          <el-button @click="visible = false" :disabled="migrating">取消</el-button>
          <el-button type="primary" @click="onSave" :loading="saving" :disabled="migrating">保存</el-button>
        </div>
      </div>
    </template>
  </el-dialog>
</template>

<script setup lang="ts">
import { computed, ref, reactive } from 'vue'
import { useRecorderStore } from '../stores/recorder'
import { DEFAULT_QUALITY, QUALITY_OPTIONS, normalizeQuality } from '../constants/quality'
import { getVersion } from '@tauri-apps/api/app'
import { invoke } from '@tauri-apps/api/core'
import { openPath } from '@tauri-apps/plugin-opener'
import { ElMessage, ElMessageBox } from 'element-plus'
import type { AppSettings, RecordingLogInfo } from '../types'

const visible = defineModel<boolean>({ default: false })
const store = useRecorderStore()
const saving = ref(false)
const migrating = ref(false)
const migrationTarget = ref('')
const hasRunningTasks = computed(() => store.tasks.some(task =>
  task.status === 'recording' || task.status === 'finalizing' || task.segments?.some(segment => ['queued', 'converting'].includes(segment.conversion_state))
))
const version = ref('')
const logInfo = ref<RecordingLogInfo | null>(null)
const logInfoError = ref('')
const loadingLogs = ref(false)
const openingLogs = ref(false)

async function refreshLogInfo(): Promise<RecordingLogInfo | null> {
  loadingLogs.value = true
  logInfoError.value = ''
  try {
    logInfo.value = await invoke<RecordingLogInfo>('get_recording_log_info')
    return logInfo.value
  } catch (error) {
    logInfo.value = null
    logInfoError.value = `无法读取日志信息：${error}`
    return null
  } finally {
    loadingLogs.value = false
  }
}

async function openLogFolder() {
  if (openingLogs.value) return
  openingLogs.value = true
  try {
    const info = await refreshLogInfo()
    if (info) await openPath(info.directory)
  } catch (error) {
    logInfoError.value = `无法打开日志文件夹：${error}`
  } finally {
    openingLogs.value = false
  }
}

getVersion().then(v => { version.value = v })

const form = reactive({
  close_behavior: 'exit' as AppSettings['close_behavior'],
  proxy: '',
  cookie: '',
  quality: DEFAULT_QUALITY,
  recordings_dir: '',
  auto_convert_mp4: false,
  segment_recording_enabled: false,
  segment_duration_minutes: 60,
  time_format_24h: true,
  time_display_mode: 'absolute',
  auto_check_interval_secs: 60,
  auto_monitor_window_hours: 6,
  auto_disable_after_record: true,
  notify_updates: true,
  log_max_size_mib: 5,
  log_backup_count: 4,
})

const logBudgetText = computed(() => {
  const size = form.log_max_size_mib
  const count = form.log_backup_count
  if (!Number.isInteger(size) || size < 1 || !Number.isInteger(count) || count < 1) return '—'
  const total = size * (count + 1)
  return total >= 1024 ? `${(total / 1024).toFixed(2)} GiB` : `${total} MiB`
})

function onOpen() {
  void refreshLogInfo()
  form.close_behavior = store.settings.close_behavior ?? 'exit'
  form.proxy = store.settings.proxy
  form.cookie = store.settings.cookie
  form.quality = normalizeQuality(store.settings.quality)
  form.recordings_dir = store.settings.recordings_dir || ''
  migrationTarget.value = ''
  form.auto_convert_mp4 = store.settings.auto_convert_mp4 ?? false
  form.segment_recording_enabled = store.settings.segment_recording_enabled ?? false
  form.segment_duration_minutes = store.settings.segment_duration_minutes ?? 60
  form.time_format_24h = store.settings.time_format_24h ?? true
  form.time_display_mode = store.settings.time_display_mode || 'absolute'
  form.auto_check_interval_secs = store.settings.auto_check_interval_secs ?? 60
  form.auto_monitor_window_hours = store.settings.auto_monitor_window_hours ?? 6
  form.auto_disable_after_record = store.settings.auto_disable_after_record ?? true
  form.notify_updates = store.settings.notify_updates ?? true
  form.log_max_size_mib = store.settings.log_max_size_mib ?? 5
  form.log_backup_count = store.settings.log_backup_count ?? 4
}

async function onMigrate() {
  if (migrating.value || saving.value || hasRunningTasks.value || !migrationTarget.value.trim()) return
  const newPath = migrationTarget.value.trim()
  migrating.value = true
  try {
    await ElMessageBox.confirm(
      '迁移会将当前数据库复制到新位置，完成后立即生效，原数据库保留。确定继续？',
      '迁移数据库',
      { confirmButtonText: '确定', cancelButtonText: '取消', type: 'warning' }
    )
    await store.migrateDb(newPath)
    migrationTarget.value = ''
    ElMessage.success('数据库已迁移并立即生效，原数据库已保留')
  } catch (e: any) {
    if (e !== 'cancel' && e !== 'close') {
      ElMessage.error(`迁移失败: ${e}`)
    }
  } finally {
    migrating.value = false
  }
}

async function onSave() {
  if (migrating.value || saving.value) return
  if (!Number.isInteger(form.segment_duration_minutes) || form.segment_duration_minutes < 1) {
    ElMessage.error('分段时长必须为正整数分钟')
    return
  }
  if (!Number.isInteger(form.log_max_size_mib) || form.log_max_size_mib < 1 || form.log_max_size_mib > 1024) {
    ElMessage.error('单个日志大小必须是 1 到 1024 MiB 的整数')
    return
  }
  if (!Number.isInteger(form.log_backup_count) || form.log_backup_count < 1 || form.log_backup_count > 100) {
    ElMessage.error('历史日志保留份数必须是 1 到 100 的整数')
    return
  }
  saving.value = true
  try {
    await store.saveSettings({ ...form, db_path: store.settings.db_path })
    ElMessage.success('设置已保存')
    visible.value = false
  } catch (e) {
    ElMessage.error(`保存失败: ${e}`)
  } finally {
    saving.value = false
  }
}
</script>

<style scoped>
.log-budget {
  margin-bottom: 10px;
}

.log-directory {
  margin-bottom: 8px;
  font-size: 12px;
  overflow-wrap: anywhere;
  color: var(--color-text-secondary);
}

.log-error {
  margin-top: 8px;
  font-size: 12px;
  overflow-wrap: anywhere;
  color: var(--color-warning);
}

.settings-body {
  padding: 4px 0;
  max-height: 66vh;
  overflow-y: auto;
  padding-right: 4px;
}

.settings-section {
  margin-bottom: 20px;
}

.settings-section:last-of-type {
  margin-bottom: 0;
}

.section-header {
  display: flex;
  align-items: baseline;
  gap: 8px;
  margin-bottom: 8px;
}

.section-label {
  font-size: 13px;
  font-weight: 600;
  color: var(--color-text);
  letter-spacing: -0.1px;
}

.section-hint {
  font-size: 11px;
  color: var(--color-text-tertiary);
}

.quality-note {
  margin-top: 7px;
  font-size: 11px;
  color: var(--color-text-tertiary);
  line-height: 1.5;
}

.hint-warn {
  color: var(--color-warning);
  font-weight: 500;
}

.db-row {
  display: flex;
  gap: 8px;
}

.db-row .el-input {
  flex: 1;
}

.db-current {
  font-size: 11px;
  color: var(--color-text-tertiary);
  margin-top: 6px;
  font-family: 'SF Mono', 'Menlo', 'Consolas', monospace;
}

.time-options {
  display: flex;
  gap: 8px;
}

.auto-settings-panel {
  border: 1px solid var(--color-border-light);
  border-radius: var(--radius-md);
  overflow: hidden;
}

.auto-setting-row {
  min-height: 66px;
  padding: 11px 12px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 14px;
  border-bottom: 1px solid var(--color-border-light);
}

.auto-setting-row:last-child {
  border-bottom: none;
}

.auto-setting-label {
  font-size: 12px;
  font-weight: 600;
  color: var(--color-text);
}

.auto-setting-hint,
.auto-settings-note,
.update-settings-note {
  font-size: 10px;
  color: var(--color-text-tertiary);
  line-height: 1.5;
}

.auto-settings-note {
  margin-top: 7px;
}

.update-settings-note {
  margin-top: 6px;
}

.number-setting {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-shrink: 0;
  font-size: 11px;
  color: var(--color-text-secondary);
}

.number-setting :deep(.el-input-number) {
  width: 112px;
}

.footer-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.version-text {
  font-size: 12px;
  color: var(--color-text-tertiary);
}
</style>
