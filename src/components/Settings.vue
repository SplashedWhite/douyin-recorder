<template>
  <el-dialog v-model="visible" title="设置" width="460" :show-close="true" @open="onOpen">
    <div class="settings-body">
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
          <span class="section-hint">不可用时自动降级</span>
        </div>
        <el-select v-model="form.quality" size="large" style="width: 100%">
          <el-option label="高清 (HD1)" value="HD1" />
          <el-option label="蓝光 (FULL_HD1)" value="FULL_HD1" />
          <el-option label="标清 (SD1)" value="SD1" />
          <el-option label="流畅 (SD2)" value="SD2" />
        </el-select>
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
              <div class="auto-setting-hint">到期仍未开播时自动停止请求</div>
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
              <div class="auto-setting-hint">仅影响由自动检测启动的录制</div>
            </div>
            <el-select v-model="form.auto_disable_after_record" style="width: 142px">
              <el-option label="关闭自动录制" :value="true" />
              <el-option label="继续下一窗口" :value="false" />
            </el-select>
          </div>
        </div>
        <div class="auto-settings-note">
          未开启自动录制的房间仍只在程序启动和手动刷新时请求状态；录制期间不会轮询。
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
          <span class="section-hint">录制完成后自动将 FLV 转为 MP4 格式</span>
        </div>
        <el-switch
          v-model="form.auto_convert_mp4"
          active-text="开启"
          inactive-text="关闭"
        />
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
          <span class="section-label">数据库位置</span>
          <span class="section-hint hint-warn">数据库极小（<1MB），不建议更改</span>
        </div>
        <div class="db-row">
          <el-input
            v-model="form.db_path"
            placeholder="D:\Data\douyin_recorder.db"
            clearable
            size="large"
          />
          <el-button size="large" @click="onMigrate" :loading="migrating" :disabled="!form.db_path">
            迁移
          </el-button>
        </div>
        <div class="db-current" v-if="store.settings.db_path">
          当前: {{ store.settings.db_path }}
        </div>
      </div>
    </div>

    <template #footer>
      <div class="footer-row">
        <span class="version-text">v{{ version }}</span>
        <div>
          <el-button @click="visible = false">取消</el-button>
          <el-button type="primary" @click="onSave" :loading="saving">保存</el-button>
        </div>
      </div>
    </template>
  </el-dialog>
</template>

<script setup lang="ts">
import { ref, reactive } from 'vue'
import { useRecorderStore } from '../stores/recorder'
import { getVersion } from '@tauri-apps/api/app'
import { ElMessage, ElMessageBox } from 'element-plus'

const visible = defineModel<boolean>({ default: false })
const store = useRecorderStore()
const saving = ref(false)
const migrating = ref(false)
const version = ref('')

getVersion().then(v => { version.value = v })

const form = reactive({
  proxy: '',
  cookie: '',
  quality: 'HD1',
  recordings_dir: '',
  db_path: '',
  auto_convert_mp4: false,
  time_format_24h: true,
  time_display_mode: 'absolute',
  auto_check_interval_secs: 60,
  auto_monitor_window_hours: 6,
  auto_disable_after_record: true,
  notify_updates: true,
})

function onOpen() {
  form.proxy = store.settings.proxy
  form.cookie = store.settings.cookie
  form.quality = store.settings.quality || 'HD1'
  form.recordings_dir = store.settings.recordings_dir || ''
  form.db_path = store.settings.db_path || ''
  form.auto_convert_mp4 = store.settings.auto_convert_mp4 ?? false
  form.time_format_24h = store.settings.time_format_24h ?? true
  form.time_display_mode = store.settings.time_display_mode || 'absolute'
  form.auto_check_interval_secs = store.settings.auto_check_interval_secs ?? 60
  form.auto_monitor_window_hours = store.settings.auto_monitor_window_hours ?? 6
  form.auto_disable_after_record = store.settings.auto_disable_after_record ?? true
  form.notify_updates = store.settings.notify_updates ?? true
}

async function onMigrate() {
  if (!form.db_path) return
  try {
    await ElMessageBox.confirm(
      '迁移会将当前数据库复制到新位置，重启后生效。确定继续？',
      '迁移数据库',
      { confirmButtonText: '确定', cancelButtonText: '取消', type: 'warning' }
    )
    migrating.value = true
    await store.migrateDb(form.db_path)
    ElMessage.success('数据库已迁移，重启后生效')
  } catch (e: any) {
    if (e !== 'cancel') {
      ElMessage.error(`迁移失败: ${e}`)
    }
  } finally {
    migrating.value = false
  }
}

async function onSave() {
  saving.value = true
  try {
    await store.saveSettings({ ...form })
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
