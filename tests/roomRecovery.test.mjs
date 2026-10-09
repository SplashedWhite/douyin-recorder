import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import * as vue from 'vue'
import { renderToString } from 'vue/server-renderer'
import { parse, compileScript, compileTemplate } from 'vue/compiler-sfc'
import ts from 'typescript'

function evaluate(source, dependencies) {
  const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText
  const exports = {}
  new Function('require', 'exports', compiled)(name => { assert.ok(name in dependencies, name); return dependencies[name] }, exports)
  return exports
}

async function fixture(recording) {
  const source = readFileSync(new URL('../src/components/RoomList.vue', import.meta.url), 'utf8')
  const { descriptor } = parse(source)
  const script = compileScript(descriptor, { id: 'room-test' })
  const template = compileTemplate({ source: descriptor.template.content, filename: 'RoomList.vue', id: 'room-test', compilerOptions: { bindingMetadata: script.bindings } })
  assert.deepEqual(template.errors, [])
  const stopped = [], cancelled = []
  const room = { id: 1, anchor_name: '主播', room_title: '直播', is_live: true, auto_monitor_mode: 'continuous', auto_record_enabled: true }
  const store = vue.reactive({
    rooms: [room], tasks: [{ id: 10, room_id: 1, status: 'finalizing' }, ...(recording ? [{ id: 11, room_id: 1, status: 'recording' }] : [])],
    recoveries: { 1: { recovery_id: 5, task_id: recording ? 11 : 10, phase: recording ? 'recording' : 'confirming', revision: 2 } },
    settings: {}, loading: false, isRefreshingAll: false,
    async cancelRecovery(id) { cancelled.push(id) },
    async stopRecord(id) { stopped.push(id); return { status: 'completed' } },
  })
  const helpers = evaluate(readFileSync(new URL('../src/utils/recovery.ts', import.meta.url), 'utf8'), {})
  const dependencies = {
    vue, pinia: { storeToRefs: value => vue.toRefs(value) }, '../utils/recovery': helpers,
    '../stores/recorder': { useRecorderStore: () => store },
    '@element-plus/icons-vue': Object.fromEntries(['Plus', 'Delete', 'VideoPlay', 'VideoPause', 'Refresh', 'Timer', 'Setting'].map(name => [name, { render: () => null }])),
    'element-plus': { ElMessage: { success() {}, error(message) { throw new Error(message) } }, ElMessageBox: {} },
  }
  const component = evaluate(script.content, dependencies).default
  component.render = evaluate(template.code, dependencies).render
  const setup = component.setup
  let state
  component.setup = (props, context) => (state = setup(props, context))
  const app = vue.createSSRApp(component)
  for (const name of new Set(source.match(/el-[a-z-]+(?=[\s/>])/g))) {
    app.component(name, { inheritAttrs: false, setup(_, { slots, attrs }) {
      return () => name === 'el-dialog' ? null : vue.h(name === 'el-button' ? 'button' : 'div', attrs, [slots.header?.(), slots.default?.()])
    } })
  }
  return { html: (await renderToString(app)).replace(/<!--[\s\S]*?-->/g, ''), state, stopped, cancelled, room }
}

test('pending recovery has a stop-recovery action with a monitoring reminder', async () => {
  const { html, state, cancelled } = await fixture(false)
  assert.match(html, /正在确认直播状态/)
  assert.match(html, /停止恢复仍保留监控/)
  assert.match(html, />停止恢复<\/button>/)
  await state.stopRecovery(1)
  assert.deepEqual(cancelled, [1])
})

test('new capture stop stays enabled while an old task is finalizing', async () => {
  const { html, state, stopped, room } = await fixture(true)
  assert.equal(state.isFinalizing(1), false)
  const button = html.match(/<button[^>]*class="record-btn"[^>]*>/)?.[0]
  assert.ok(button)
  assert.doesNotMatch(button, /disabled/)
  assert.doesNotMatch(html, />停止恢复<\/button>/)
  await state.toggleRecord(room)
  assert.deepEqual(stopped, [11])
})
