<template>
  <div id="app">
    <div class="app-container">
      <header class="app-header">
        <div class="header-content">
          <div class="header-left">
            <div class="logo-icon">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <circle cx="12" cy="12" r="10" />
                <polygon points="10,8 16,12 10,16" fill="currentColor" stroke="none" />
              </svg>
            </div>
            <h1>抖音直播录制</h1>
          </div>
          <div class="header-actions">
            <el-button
              v-if="store.availableUpdate"
              type="primary"
              plain
              round
              size="small"
              class="update-btn"
              :title="`发现新版本 v${store.availableUpdate.latest_version}`"
              @click="openLatestRelease"
            >
              <el-icon><TopRight /></el-icon>
              有更新 v{{ store.availableUpdate.latest_version }}
            </el-button>
            <el-button text circle @click="showSettings = true" class="settings-btn">
              <el-icon :size="20"><Setting /></el-icon>
            </el-button>
          </div>
        </div>
      </header>

      <main class="app-main">
        <RoomList />
        <TaskList />
      </main>

      <Settings v-model="showSettings" />
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted, onBeforeUnmount } from 'vue'
import { Setting, TopRight } from '@element-plus/icons-vue'
import { openUrl } from '@tauri-apps/plugin-opener'
import { ElMessage } from 'element-plus'
import { useRecorderStore } from './stores/recorder'
import RoomList from './components/RoomList.vue'
import TaskList from './components/TaskList.vue'
import Settings from './components/Settings.vue'

const store = useRecorderStore()
const showSettings = ref(false)

async function openLatestRelease() {
  const update = store.availableUpdate
  if (!update) return

  try {
    await openUrl(update.release_url)
  } catch (e) {
    ElMessage.error(`无法打开更新页面: ${e}`)
  }
}

onMounted(async () => {
  await store.listenRecordingEvents()
  await Promise.all([
    store.loadRooms(),
    store.loadTasks(),
    store.loadSettings(),
  ])
  void store.checkForUpdate()
  // 启动时自动刷新未开启自动录制的直播间状态
  void store.refreshAllRooms()
})

onBeforeUnmount(() => {
  store.stopListeningRecordingEvents()
})
</script>

<style>
:root {
  --color-bg: #f5f5f7;
  --color-surface: #ffffff;
  --color-surface-hover: #f8f8fa;
  --color-text: #1d1d1f;
  --color-text-secondary: #6e6e73;
  --color-text-tertiary: #aeaeb2;
  --color-border: #e8e8ed;
  --color-border-light: #f0f0f2;
  --color-primary: #0071e3;
  --color-primary-hover: #0077ed;
  --color-primary-light: rgba(0, 113, 227, 0.08);
  --color-success: #34c759;
  --color-success-light: rgba(52, 199, 89, 0.1);
  --color-warning: #ff9500;
  --color-warning-light: rgba(255, 149, 0, 0.1);
  --color-danger: #ff3b30;
  --color-danger-light: rgba(255, 59, 48, 0.08);

  --radius-xs: 6px;
  --radius-sm: 8px;
  --radius-md: 12px;
  --radius-lg: 16px;
  --radius-xl: 20px;
  --radius-full: 9999px;

  --shadow-xs: 0 1px 2px rgba(0, 0, 0, 0.04);
  --shadow-sm: 0 1px 3px rgba(0, 0, 0, 0.06), 0 1px 2px rgba(0, 0, 0, 0.04);
  --shadow-md: 0 4px 12px rgba(0, 0, 0, 0.08), 0 1px 3px rgba(0, 0, 0, 0.04);
  --shadow-lg: 0 8px 24px rgba(0, 0, 0, 0.1), 0 2px 6px rgba(0, 0, 0, 0.04);

  --transition-fast: 0.15s ease;
  --transition-normal: 0.25s ease;
  --transition-slow: 0.35s cubic-bezier(0.4, 0, 0.2, 1);
}

* {
  margin: 0;
  padding: 0;
  box-sizing: border-box;
}

html {
  scroll-behavior: smooth;
}

body {
  font-family: -apple-system, BlinkMacSystemFont, 'SF Pro Display', 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', sans-serif;
  background: var(--color-bg);
  color: var(--color-text);
  -webkit-font-smoothing: antialiased;
  -moz-osx-font-smoothing: grayscale;
  line-height: 1.5;
}

#app {
  min-height: 100vh;
}

.app-container {
  max-width: 860px;
  margin: 0 auto;
  padding: 0 20px 48px;
}

/* ── Frosted Glass Header ── */
.app-header {
  padding: 16px 0 20px;
  position: sticky;
  top: 0;
  z-index: 100;
  background: rgba(245, 245, 247, 0.72);
  -webkit-backdrop-filter: saturate(180%) blur(20px);
  backdrop-filter: saturate(180%) blur(20px);
}

.header-content {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.header-left {
  display: flex;
  align-items: center;
  gap: 10px;
}

.logo-icon {
  width: 34px;
  height: 34px;
  background: linear-gradient(135deg, #fe2c55 0%, #ff6b81 100%);
  border-radius: var(--radius-sm);
  display: flex;
  align-items: center;
  justify-content: center;
  color: white;
  box-shadow: 0 2px 8px rgba(254, 44, 85, 0.25);
  flex-shrink: 0;
}

.logo-icon svg {
  width: 18px;
  height: 18px;
}

.app-header h1 {
  font-size: 20px;
  font-weight: 600;
  letter-spacing: -0.4px;
  color: var(--color-text);
}

.header-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}

.update-btn {
  border-radius: var(--radius-full) !important;
  padding: 6px 10px !important;
}

.settings-btn {
  color: var(--color-text-tertiary) !important;
  transition: all var(--transition-fast) !important;
  width: 34px !important;
  height: 34px !important;
}

.settings-btn:hover {
  color: var(--color-text) !important;
  background: rgba(0, 0, 0, 0.05) !important;
}

/* ── Main Layout ── */
.app-main {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

/* ── Element Plus: Cards ── */
.el-card {
  border: none !important;
  border-radius: var(--radius-lg) !important;
  box-shadow: var(--shadow-sm) !important;
  background: var(--color-surface) !important;
  overflow: hidden;
  transition: box-shadow var(--transition-normal) !important;
}

.el-card:hover {
  box-shadow: var(--shadow-md) !important;
}

.el-card__header {
  padding: 16px 20px !important;
  border-bottom: 1px solid var(--color-border-light) !important;
  background: var(--color-surface) !important;
}

.el-card__body {
  padding: 0 !important;
}

/* ── Element Plus: Buttons ── */
.el-button {
  border-radius: var(--radius-sm) !important;
  font-weight: 500 !important;
  font-size: 13px !important;
  transition: all var(--transition-fast) !important;
  letter-spacing: -0.1px;
}

.el-button--primary {
  --el-button-bg-color: var(--color-primary);
  --el-button-border-color: var(--color-primary);
  --el-button-hover-bg-color: var(--color-primary-hover);
  --el-button-hover-border-color: var(--color-primary-hover);
}

.el-button--danger {
  --el-button-bg-color: var(--color-danger);
  --el-button-border-color: var(--color-danger);
}

.el-button--warning {
  --el-button-bg-color: var(--color-warning);
  --el-button-border-color: var(--color-warning);
}

.el-button--small {
  font-size: 12px !important;
  padding: 6px 12px !important;
}

/* ── Element Plus: Dialogs ── */
.el-dialog {
  border-radius: var(--radius-lg) !important;
  overflow: hidden;
  box-shadow: var(--shadow-lg) !important;
}

.el-dialog__header {
  padding: 20px 24px 12px !important;
  margin-right: 0 !important;
}

.el-dialog__title {
  font-weight: 600 !important;
  font-size: 17px !important;
  letter-spacing: -0.3px;
}

.el-dialog__body {
  padding: 0 24px 8px !important;
}

.el-dialog__footer {
  padding: 12px 24px 20px !important;
}

/* ── Element Plus: Inputs ── */
.el-input__wrapper {
  border-radius: var(--radius-sm) !important;
  box-shadow: 0 0 0 1px var(--color-border) inset !important;
  padding: 4px 12px !important;
  transition: all var(--transition-fast) !important;
}

.el-input__wrapper:hover {
  box-shadow: 0 0 0 1px var(--color-text-tertiary) inset !important;
}

.el-input__wrapper.is-focus {
  box-shadow: 0 0 0 2px var(--color-primary) inset !important;
}

.el-select .el-input__wrapper {
  border-radius: var(--radius-sm) !important;
}

/* ── Element Plus: Tags ── */
.el-tag {
  border-radius: var(--radius-xs) !important;
  font-size: 11px !important;
  font-weight: 500 !important;
  letter-spacing: 0.2px;
}
</style>
