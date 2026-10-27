<template>
  <div class="live-win">
    <div class="live-topbar" @mousedown="onTopbarMouseDown">
      <span class="live-top-title">{{ title }}</span>
      <div class="live-top-actions">
        <button class="lw-btn" :title="muted ? '取消静音' : '静音'" @click="toggleMute">
          <el-icon v-if="muted"><Mute /></el-icon>
          <el-icon v-else><Microphone /></el-icon>
        </button>
        <button class="lw-btn" title="最小化" @click="minimize">
          <el-icon><Minus /></el-icon>
        </button>
        <button class="lw-btn" :title="isMax ? '还原' : '放大'" @click="toggleMaximize">
          <el-icon v-if="isMax"><CopyDocument /></el-icon>
          <el-icon v-else><FullScreen /></el-icon>
        </button>
        <button class="lw-btn lw-close" title="关闭" @click="close">
          <el-icon><Close /></el-icon>
        </button>
      </div>
    </div>

    <div class="live-stage">
      <video ref="videoRef" class="live-video" muted autoplay playsinline></video>

      <div v-if="state !== 'playing'" class="live-overlay">
        <el-icon v-if="state === 'loading'" class="spin"><Loading /></el-icon>
        <div v-else-if="state === 'error'" class="err">
          <el-icon><WarningFilled /></el-icon>
          <span>{{ errMsg }}</span>
        </div>
        <div v-else>准备中…</div>
      </div>
    </div>

    <!-- 八向缩放热区：四角 + 四边，统一按比例整体缩放（无边框窗口无系统手柄，自行实现） -->
    <div class="resize-handle rh-nw" @pointerdown="startResize('nw', $event)"></div>
    <div class="resize-handle rh-ne" @pointerdown="startResize('ne', $event)"></div>
    <div class="resize-handle rh-sw" @pointerdown="startResize('sw', $event)"></div>
    <div class="resize-handle rh-se" @pointerdown="startResize('se', $event)"></div>
    <div class="resize-handle rh-n" @pointerdown="startResize('n', $event)"></div>
    <div class="resize-handle rh-s" @pointerdown="startResize('s', $event)"></div>
    <div class="resize-handle rh-w" @pointerdown="startResize('w', $event)"></div>
    <div class="resize-handle rh-e" @pointerdown="startResize('e', $event)"></div>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted, onBeforeUnmount, nextTick } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { getCurrentWindow, LogicalSize, LogicalPosition } from '@tauri-apps/api/window'
import { Mute, Microphone, Minus, FullScreen, CopyDocument, Close, Loading, WarningFilled } from '@element-plus/icons-vue'
import flvjs from 'flv.js'

const win = getCurrentWindow()
const label = win.label
const roomId = Number(label.replace(/^live-/, '')) || 0
const title = ref(`房间 ${roomId}`)

const videoRef = ref<HTMLVideoElement | null>(null)
const state = ref<'loading' | 'playing' | 'error'>('loading')
const errMsg = ref('')
const muted = ref(true)
const isMax = ref(false)
let flvPlayer: flvjs.Player | null = null
let unlistenResize: (() => void) | null = null
let dragging = false
const TOPBAR = 24
const cachedSf = { v: 1 }
let rafId = 0
let resizePending: { nw: number; nh: number; nx: number; ny: number } | null = null
function flushResize() {
  rafId = 0
  if (!resizePending) return
  const { nw, nh, nx, ny } = resizePending
  resizePending = null
  win.setSize(new LogicalSize(Math.round(nw), Math.round(nh))).catch(() => {})
  win.setPosition(new LogicalPosition(Math.round(nx), Math.round(ny))).catch(() => {})
}
function scheduleResize(nw: number, nh: number, nx: number, ny: number) {
  resizePending = { nw, nh, nx, ny }
  if (!rafId) rafId = requestAnimationFrame(flushResize)
}

// 视频真实宽高比，未就绪回退 9:16
function videoAspect(): number {
  const v = videoRef.value
  if (v && v.videoWidth && v.videoHeight) return v.videoWidth / v.videoHeight
  return 9 / 16
}

// 窗口尺寸贴合视频比例，避免黑边
const TARGET_LONG = 560
function fitWindowToVideo() {
  const v = videoRef.value
  if (!v || !v.videoWidth || !v.videoHeight) return
  const aspect = v.videoWidth / v.videoHeight
  let w: number, h: number
  if (aspect >= 1) {
    w = TARGET_LONG
    h = Math.round(TARGET_LONG / aspect) + TOPBAR
  } else {
    h = TARGET_LONG + TOPBAR
    w = Math.round(TARGET_LONG * aspect)
  }
  win.setSize(new LogicalSize(w, h)).catch(() => {})
}

// resize 时锁定视频比例（实时取 scaleFactor，避免缓存错位）
async function enforceAspect() {
  if (dragging) return
  const v = videoRef.value
  if (!v || !v.videoWidth || !v.videoHeight) return
  try {
    const sf = await win.scaleFactor()
    const ps = await win.innerSize()
    const w = ps.width / sf
    const h = ps.height / sf
    const aspect = v.videoWidth / v.videoHeight
    const vhAvail = h - TOPBAR
    let vw = w
    let vh = w / aspect
    if (vh > vhAvail) { vh = vhAvail; vw = vh * aspect }
    const nw = Math.round(vw)
    const nh = Math.round(vh + TOPBAR)
    if (Math.abs(nw - w) <= 1 && Math.abs(nh - h) <= 1) return
    await win.setSize(new LogicalSize(nw, nh))
  } catch { /* noop */ }
}

// 放大到屏幕并保持比例（非系统最大化，避免左右黑边）
async function zoomToScreen() {
  const aspect = videoAspect()
  const availW = window.screen.availWidth
  const availH = window.screen.availHeight - TOPBAR
  let vw: number, vh: number
  if (aspect >= 1) {
    vw = availW
    vh = vw / aspect
    if (vh > availH) { vh = availH; vw = vh * aspect }
  } else {
    vh = availH
    vw = vh * aspect
    if (vw > availW) { vw = availW; vh = vw / aspect }
  }
  try {
    await win.setSize(new LogicalSize(Math.round(vw), Math.round(vh) + TOPBAR))
    try { await win.center() } catch { /* noop */ }
  } catch { /* noop */ }
  isMax.value = true
}
function restoreSize() {
  fitWindowToVideo()
  isMax.value = false
}

// 八向拖动缩放：四边与四角同为整体按比例放大缩小，拖到屏幕边自动停
async function startResize(dir: string, e: PointerEvent) {
  e.preventDefault()
  e.stopPropagation()
  dragging = true
  const aspect = videoAspect()
  const ps = await win.innerSize().catch(() => null)
  const pp = await win.innerPosition().catch(() => null)
  if (!ps || !pp) { dragging = false; return }
  const sf = (await win.scaleFactor().catch(() => null)) || cachedSf.v
  cachedSf.v = sf
  const startW = ps.width / sf
  const startH = ps.height / sf
  const startX = pp.x / sf
  const startY = pp.y / sf
  const startX0 = e.clientX
  const startY0 = e.clientY
  const maxW = window.screen.availWidth
  const maxH = window.screen.availHeight
  const isHoriz = dir === 'w' || dir === 'e'
  const isCorner = dir.length === 2
  const move = (ev: PointerEvent) => {
    const dx = ev.clientX - startX0
    const dy = ev.clientY - startY0
    // 向外放大：右/下为正向，左/上取负
    const dX = dir.includes('w') ? -dx : dx
    const dY = dir.includes('n') ? -dy : dy
    // 纯横边以宽驱动；纯竖边以高驱动；角取位移较大轴驱动
    const useX = isHoriz || (isCorner && Math.abs(dx) >= Math.abs(dy))
    let vw: number, vh: number
    if (useX) { vw = startW + dX; vh = vw / aspect }
    else { vh = (startH - TOPBAR) + dY; vw = vh * aspect }
    // 触边即停（保持比例）
    if (vw > maxW) { vw = maxW; vh = vw / aspect }
    if (vh > maxH - TOPBAR) { vh = maxH - TOPBAR; vw = vh * aspect }
    // 最小尺寸
    if (vw < 200) { vw = 200; vh = vw / aspect }
    if (vh < 156) { vh = 156; vw = vh * aspect }
    const nw = vw
    const nh = vh + TOPBAR
    // 锚点：被拖对边固定（整体缩放）
    let nx = startX
    let ny = startY
    if (dir.includes('w')) nx = startX + (startW - nw)
    if (dir.includes('n')) ny = startY + (startH - nh)
    nx = Math.max(0, Math.min(nx, maxW - nw))
    ny = Math.max(0, Math.min(ny, maxH - nh))
    scheduleResize(nw, nh, nx, ny)
  }
  const up = () => {
    dragging = false
    if (rafId) { cancelAnimationFrame(rafId); rafId = 0 }
    if (resizePending) flushResize()
    document.removeEventListener('pointermove', move)
    document.removeEventListener('pointerup', up)
  }
  document.addEventListener('pointermove', move)
  document.addEventListener('pointerup', up)
}

function destroyPlayer() {
  if (flvPlayer) {
    try { flvPlayer.destroy() } catch { /* noop */ }
    flvPlayer = null
  }
}

async function startPlayback(url: string) {
  await nextTick()
  const video = videoRef.value
  if (!video) return
  if (!flvjs.isSupported()) {
    state.value = 'error'
    errMsg.value = '当前环境不支持 flv 播放'
    return
  }
  destroyPlayer()
  const mediaDataSource = {
    type: 'flv',
    url,
    isLive: true,
    cors: true,
    hasAudio: true,
    xhrSetup(xhr: XMLHttpRequest) {
      xhr.setRequestHeader('Referer', 'https://live.douyin.com/')
      xhr.setRequestHeader('User-Agent', navigator.userAgent)
    },
  } as flvjs.MediaDataSource
  const player = flvjs.createPlayer(mediaDataSource, {
    enableStashBuffer: true,
    stashInitialSize: 128,
  })
  video.removeEventListener('loadedmetadata', fitWindowToVideo)
  video.addEventListener('loadedmetadata', fitWindowToVideo)
  player.attachMediaElement(video)
  player.on(flvjs.Events.ERROR, () => {
    if (state.value !== 'playing') return
    state.value = 'error'
    errMsg.value = '直播流加载失败（可能被防盗链拦截）'
  })
  player.load()
  try {
    await video.play()
  } catch { /* 静音自动播放通常可用 */ }
  flvPlayer = player
  state.value = 'playing'
  setTimeout(fitWindowToVideo, 400)
}

onMounted(async () => {
  // 取主播名作标题
  try {
    const name = await invoke<string>('get_anchor_name', { roomId })
    if (name) title.value = `${name} 的直播间`
  } catch { /* 失败保留房间号占位 */ }
  try {
    cachedSf.v = await win.scaleFactor()
  } catch { /* 回退 1 */ }
  try {
    unlistenResize = await win.onResized(() => enforceAspect())
  } catch { /* noop */ }
  if (!roomId) {
    state.value = 'error'
    errMsg.value = '无法识别直播间'
    return
  }
  try {
    const url = await invoke<string>('get_live_preview_url', { roomId })
    await startPlayback(url)
  } catch (e: any) {
    state.value = 'error'
    errMsg.value = String(e) || '预览不可用'
  }
})

onBeforeUnmount(() => {
  if (unlistenResize) {
    unlistenResize()
    unlistenResize = null
  }
  destroyPlayer()
})

// 顶部栏拖动
function onTopbarMouseDown(e: MouseEvent) {
  const target = e.target as HTMLElement
  if (target.closest('.live-top-actions')) return
  win.startDragging().catch(() => {})
}

function toggleMute() {
  const v = videoRef.value
  if (!v) return
  v.muted = !v.muted
  muted.value = v.muted
}
async function minimize() {
  try { await win.minimize() } catch { /* noop */ }
}
function toggleMaximize() {
  if (isMax.value) restoreSize()
  else zoomToScreen()
}
async function close() {
  try { await win.close() } catch { /* noop */ }
}
</script>

<style scoped>
.live-win {
  position: fixed;
  inset: 0;
  display: flex;
  flex-direction: column;
  background: #000;
  overflow: hidden;
}
.live-topbar {
  flex: 0 0 auto;
  height: 24px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 6px;
  background: rgba(0, 0, 0, 0.35);
  color: #fff;
  user-select: none;
  cursor: default;
  transition: background 0.15s ease;
}
.live-topbar:hover {
  background: rgba(0, 0, 0, 0.6);
}
.live-top-title {
  font-size: 12px;
  opacity: 0.75;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.live-top-actions {
  display: flex;
  gap: 4px;
}
.lw-btn {
  width: 24px;
  height: 20px;
  border: none;
  border-radius: 4px;
  background: rgba(255, 255, 255, 0.12);
  color: #fff;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: background 0.15s ease;
}
.lw-btn:hover {
  background: rgba(255, 255, 255, 0.28);
}
.lw-close:hover {
  background: #ff3b30;
}
.live-stage {
  position: relative;
  flex: 1 1 auto;
  min-height: 0;
  background: #000;
}
/* cover 填满无黑边；窗口已贴合比例，拖动变形时以裁剪方式填满 */
.live-video {
  width: 100%;
  height: 100%;
  object-fit: cover;
  background: #000;
  display: block;
}
.live-overlay {
  position: absolute;
  inset: 0;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 12px;
  color: #fff;
  background: rgba(0, 0, 0, 0.6);
  font-size: 14px;
}
.live-overlay .err {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  color: #ff6b81;
  max-width: 80%;
  text-align: center;
  line-height: 1.6;
}
.live-overlay .spin {
  animation: lw-spin 1s linear infinite;
  font-size: 28px;
}
@keyframes lw-spin {
  to { transform: rotate(360deg); }
}
.resize-handle {
  position: absolute;
  z-index: 20;
  opacity: 0;
  transition: opacity 0.15s ease, background 0.15s ease;
  touch-action: none;
  user-select: none;
}
.live-win:hover .resize-handle {
  opacity: 0.45;
}
.resize-handle:hover {
  opacity: 1 !important;
  background: rgba(255, 255, 255, 0.55);
}
/* 四角 */
.rh-nw { top: 0; left: 0; width: 16px; height: 16px; cursor: nwse-resize; }
.rh-ne { top: 0; right: 0; width: 16px; height: 16px; cursor: nesw-resize; }
.rh-sw { bottom: 0; left: 0; width: 16px; height: 16px; cursor: nesw-resize; }
.rh-se { bottom: 0; right: 0; width: 16px; height: 16px; cursor: nwse-resize; }
/* 四边：细长条，避开角热区，拖四边 = 整体按比例缩放 */
.rh-n { top: 0; left: 16px; right: 16px; height: 10px; cursor: ns-resize; }
.rh-s { bottom: 0; left: 16px; right: 16px; height: 10px; cursor: ns-resize; }
.rh-w { left: 0; top: 16px; bottom: 16px; width: 10px; cursor: ew-resize; }
.rh-e { right: 0; top: 16px; bottom: 16px; width: 10px; cursor: ew-resize; }
</style>
