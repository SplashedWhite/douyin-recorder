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
      <el-button @click="visible = false">取消</el-button>
      <el-button type="primary" @click="onSave" :loading="saving">保存</el-button>
    </template>
  </el-dialog>
</template>

<script setup lang="ts">
import { ref, reactive } from 'vue'
import { useRecorderStore } from '../stores/recorder'
import { ElMessage, ElMessageBox } from 'element-plus'

const visible = defineModel<boolean>({ default: false })
const store = useRecorderStore()
const saving = ref(false)
const migrating = ref(false)

const form = reactive({
  proxy: '',
  cookie: '',
  quality: 'HD1',
  recordings_dir: '',
  db_path: '',
  auto_convert_mp4: false,
  time_format_24h: true,
  time_display_mode: 'absolute',
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
</style>
