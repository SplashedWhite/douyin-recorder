import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import * as vue from 'vue'
import { parse, compileScript, compileTemplate } from 'vue/compiler-sfc'
import { renderToString } from 'vue/server-renderer'
import ts from 'typescript'

// Exercise the real Settings component with native calls replaced by local fixtures.
// Vue and its template compiler are already dependencies of the application.
const source = readFileSync(new URL('../src/components/Settings.vue', import.meta.url), 'utf8')
const { descriptor } = parse(source)
const script = compileScript(descriptor, { id: 'settings-test' })
const template = compileTemplate({
  source: descriptor.template.content, filename: 'Settings.vue', id: 'settings-test',
  compilerOptions: { bindingMetadata: script.bindings },
})
assert.deepEqual(template.errors, [])

function evaluate(source, dependencies) {
  const compiled = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText
  const exports = {}
  new Function('require', 'exports', compiled)(name => {
    assert.ok(name in dependencies, `unexpected dependency: ${name}`)
    return dependencies[name]
  }, exports)
  return exports
}

async function settingsFixture(overrides = {}, logOverrides = {}) {
  const saved = [], opened = [], errors = [], commands = []
  const store = {
    settings: { quality: 'ORIGIN', db_path: 'test.db', log_max_size_mib: 5, log_backup_count: 4, ...overrides },
    tasks: [],
    async saveSettings(settings) { saved.push({ ...settings }); this.settings = { ...settings } },
  }
  const dependencies = {
    vue,
    '../stores/recorder': { useRecorderStore: () => store },
    '../constants/quality': { DEFAULT_QUALITY: 'ORIGIN', QUALITY_OPTIONS: [], normalizeQuality: value => value || 'ORIGIN' },
    '@tauri-apps/api/app': { getVersion: async () => 'test' },
    '@tauri-apps/api/core': { invoke: async command => {
      commands.push(command)
      assert.equal(command, 'get_recording_log_info')
      return { directory: 'test-logs', last_error: null, api_last_error: null, ...logOverrides }
    } },
    '@tauri-apps/plugin-opener': { openPath: async path => opened.push(path) },
    'element-plus': { ElMessage: { error: value => errors.push(value), success() {} }, ElMessageBox: {} },
  }
  const component = evaluate(script.content, dependencies).default
  component.render = evaluate(template.code, dependencies).render
  let state
  const setup = component.setup
  component.setup = (props, context) => {
    state = setup(props, context)
    state.onOpen()
    return state
  }
  const app = vue.createSSRApp(component, { modelValue: true })
  for (const name of new Set(source.match(/el-[a-z-]+(?=[\s/>])/g))) {
    app.component(name, {
      inheritAttrs: false,
      setup(_, { slots, attrs }) {
        return () => vue.h('div', { 'aria-label': attrs['aria-label'] }, [slots.default?.(), slots.footer?.()])
      },
    })
  }
  const html = await renderToString(app)
  return { state, html, store, saved, opened, errors, commands }
}

test('legacy settings default to disabled API capture and hide its independent limits', async () => {
  const { state, html } = await settingsFixture()
  assert.equal(state.form.api_log_enabled, false)
  assert.equal(state.form.api_log_max_size_mib, 5)
  assert.equal(state.form.api_log_backup_count, 4)
  assert.match(html, /记录接口响应/)
  assert.doesNotMatch(html, /aria-label="单个接口日志大小"/)
  assert.match(html, /douyin-api\.log/)
})

test('enabled API capture renders separate limits and saves them without changing recording limits', async () => {
  const fixture = await settingsFixture({ api_log_enabled: true, api_log_max_size_mib: 20, api_log_backup_count: 6 })
  const { state, html, saved } = fixture
  assert.match(html, /aria-label="单个接口日志大小"/)
  assert.match(html, /aria-label="接口日志历史保留份数"/)
  assert.match(html, /140 MiB/)
  state.form.api_log_max_size_mib = 30
  state.form.api_log_backup_count = 2
  await state.onSave()
  assert.equal(saved.length, 1)
  assert.equal(saved[0].api_log_max_size_mib, 30)
  assert.equal(saved[0].api_log_backup_count, 2)
  assert.equal(saved[0].log_max_size_mib, 5)
  assert.equal(saved[0].log_backup_count, 4)
  assert.equal(saved[0].db_path, 'test.db')
  state.onOpen()
  assert.equal(state.form.api_log_max_size_mib, 30)
  state.form.api_log_enabled = false
  await state.onSave()
  assert.equal(saved[1].api_log_enabled, false)
  assert.equal(saved[1].api_log_max_size_mib, 30)
})

test('invalid API limits prevent saving and the existing folder entry reports both log errors', async () => {
  const { state, saved, errors, opened, commands } = await settingsFixture({}, { last_error: 'recording failed', api_last_error: 'api failed' })
  for (const [size, count] of [[0, 4], [1025, 4], [1.5, 4], [5, 0], [5, 101], [5, 1.5]]) {
    state.form.api_log_max_size_mib = size
    state.form.api_log_backup_count = count
    await state.onSave()
  }
  assert.equal(saved.length, 0)
  assert.equal(errors.length, 6)
  assert.equal(state.logInfo.value.last_error, 'recording failed')
  assert.equal(state.logInfo.value.api_last_error, 'api failed')
  await state.openLogFolder()
  assert.deepEqual(opened, ['test-logs'])
  assert.deepEqual(commands, ['get_recording_log_info', 'get_recording_log_info'])
})
