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

async function settingsFixture(overrides = {}, logOverrides = {}, autostartCapability = true) {
  const saved = [], opened = [], errors = [], commands = [], successes = [], visibility = []
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
      if (command === 'is_autostart_available') {
        if (autostartCapability instanceof Error) throw autostartCapability
        return autostartCapability
      }
      assert.equal(command, 'get_recording_log_info')
      return { directory: 'test-logs', last_error: null, api_last_error: null, ...logOverrides }
    } },
    '@tauri-apps/plugin-opener': { openPath: async path => opened.push(path) },
    'element-plus': { ElMessage: { error: value => errors.push(value), success: value => successes.push(value) }, ElMessageBox: {} },
  }
  const component = evaluate(script.content, dependencies).default
  component.render = evaluate(template.code, dependencies).render
  let state
  const setup = component.setup
  component.setup = async (props, context) => {
    state = setup(props, context)
    await state.onOpen()
    return state
  }
  const app = vue.createSSRApp(component, {
    modelValue: true,
    'onUpdate:modelValue': value => visibility.push(value),
  })
  for (const name of new Set(source.match(/el-[a-z-]+(?=[\s/>])/g))) {
    app.component(name, {
      inheritAttrs: false,
      setup(_, { slots, attrs }) {
        return () => vue.h('div', {
          'aria-label': attrs['aria-label'],
          'aria-disabled': attrs.disabled === undefined ? undefined : String(attrs.disabled),
        }, [slots.default?.(), slots.footer?.()])
      },
    })
  }
  const html = await renderToString(app)
  return { state, html, store, saved, opened, errors, commands, successes, visibility }
}

test('autostart save failure stays open without success feedback and can be retried', async () => {
  const { state, store, saved, errors, successes, visibility } =
    await settingsFixture({ launch_on_startup: false })
  const save = store.saveSettings
  store.saveSettings = async () => { throw new Error('设置系统自启动失败: 访问被拒绝') }
  state.form.launch_on_startup = true
  await state.onSave()
  assert.equal(store.settings.launch_on_startup, false)
  assert.equal(saved.length, 0)
  assert.equal(successes.length, 0)
  assert.deepEqual(visibility, [])
  assert.match(errors[0], /设置系统自启动失败: 访问被拒绝/)
  assert.equal(state.form.launch_on_startup, true)
  assert.equal(state.saving.value, false)
  store.saveSettings = save
  await state.onSave()
  assert.equal(saved.at(-1).launch_on_startup, true)
  assert.deepEqual(successes, ['设置已保存'])
  assert.deepEqual(visibility, [false])
})

test('debug mode disables autostart and preserves its preference when saving other settings', async () => {
  for (const enabled of [true, false, undefined]) {
    const { state, html, saved, commands } =
      await settingsFixture({ launch_on_startup: enabled }, {}, false)
    assert.ok(commands.includes('is_autostart_available'))
    assert.match(html, /开发模式不修改系统自启动/)
    assert.match(html, /aria-label="随系统启动" aria-disabled="true"/)
    state.form.launch_on_startup = !enabled
    state.form.quality = 'HD1'
    await state.onSave()
    assert.equal(saved.at(-1).launch_on_startup, enabled ?? false)
    assert.equal(saved.at(-1).quality, 'HD1')
  }
})

test('release mode defaults autostart off and preserves explicit opt-in', async () => {
  const { state, html, saved } = await settingsFixture()
  assert.doesNotMatch(html, /开发模式不修改系统自启动/)
  assert.match(html, /aria-label="随系统启动" aria-disabled="false"/)
  assert.match(html, /默认关闭/)
  assert.equal(state.form.launch_on_startup, false)
  await state.onSave()
  assert.equal(saved.at(-1).launch_on_startup, false)
  state.form.launch_on_startup = true
  await state.onSave()
  assert.equal(saved.at(-1).launch_on_startup, true)
  const reopened = await settingsFixture(saved.at(-1))
  assert.equal(reopened.state.form.launch_on_startup, true)
})

test('capability lookup failure disables only autostart and retains its saved value', async () => {
  const { state, html, saved } =
    await settingsFixture({ launch_on_startup: false }, {}, new Error('backend unavailable'))
  assert.match(html, /无法读取自启动可用状态/)
  assert.match(html, /aria-label="随系统启动" aria-disabled="true"/)
  state.form.launch_on_startup = true
  state.form.quality = 'SD1'
  await state.onSave()
  assert.equal(saved.at(-1).launch_on_startup, false)
  assert.equal(saved.at(-1).quality, 'SD1')
})

test('fixed anchor folders default off, explain rename behavior, and persist opt-in', async () => {
  const { state, html, saved } = await settingsFixture({ recordings_dir: 'recordings', auto_convert_mp4: true })
  assert.equal(state.form.organize_recordings_by_anchor, false)
  assert.match(html, /aria-label="按主播分文件夹"/)
  assert.match(html, /主播改名后继续使用/)
  assert.match(html, /已有文件保留原位置/)
  for (const enabled of [true, false, true]) {
    state.form.organize_recordings_by_anchor = enabled
    await state.onSave()
    assert.equal(saved.at(-1).organize_recordings_by_anchor, enabled)
    assert.equal(saved.at(-1).recordings_dir, 'recordings')
    assert.equal(saved.at(-1).auto_convert_mp4, true)
    await state.onOpen()
    assert.equal(state.form.organize_recordings_by_anchor, enabled)
  }
  const reopened = await settingsFixture(saved.at(-1))
  assert.equal(reopened.state.form.organize_recordings_by_anchor, true)
})

test('legacy settings default to disabled API capture and hide its independent limits', async () => {
  const { state, html } = await settingsFixture()
  assert.equal(state.form.api_log_enabled, false)
  assert.equal(state.form.api_log_max_size_mib, 5)
  assert.equal(state.form.api_log_backup_count, 4)
  assert.match(html, /记录接口响应/)
  assert.doesNotMatch(html, /aria-label="单个接口日志大小"/)
  assert.match(html, /douyin-api\.log/)
})

test('network settings default off and preserve opt-in with custom seconds for future recordings', async () => {
  const { state, html, saved } = await settingsFixture()
  assert.equal(state.form.ffmpeg_reconnect_enabled, false)
  assert.equal(state.form.ffmpeg_rw_timeout_secs, 20)
  assert.equal(state.form.ffmpeg_reconnect_max_retries, 5)
  assert.equal(state.form.ffmpeg_reconnect_delay_max_secs, 15)
  assert.equal(state.form.ffmpeg_reconnect_delay_total_max_secs, 30)
  assert.match(html, /aria-label="网络读写超时"/)
  assert.doesNotMatch(html, /aria-label="重连次数限制"/)
  const enabled = await settingsFixture({ ffmpeg_reconnect_enabled: true })
  assert.equal(enabled.state.form.ffmpeg_reconnect_enabled, true)
  assert.match(enabled.html, /aria-label="重连次数限制"/)
  assert.match(html, /保存后对新启动的录制生效/)
  state.form.ffmpeg_reconnect_enabled = true
  state.form.ffmpeg_rw_timeout_secs = 15
  state.form.ffmpeg_reconnect_max_retries = 3
  state.form.ffmpeg_reconnect_delay_max_secs = 8
  state.form.ffmpeg_reconnect_delay_total_max_secs = 25
  await state.onSave()
  assert.equal(saved.length, 1)
  assert.equal(saved[0].ffmpeg_reconnect_enabled, true)
  assert.equal(saved[0].ffmpeg_rw_timeout_secs, 15)
  assert.equal(saved[0].ffmpeg_reconnect_max_retries, 3)
  assert.equal(saved[0].ffmpeg_reconnect_delay_max_secs, 8)
  assert.equal(saved[0].ffmpeg_reconnect_delay_total_max_secs, 25)
  await state.onOpen()
  assert.equal(state.form.ffmpeg_reconnect_enabled, true)
  assert.equal(state.form.ffmpeg_rw_timeout_secs, 15)
})

test('software recovery defaults off, preserves opt-in and custom bounds, and rejects invalid saves', async () => {
  const { state, html, saved, errors } = await settingsFixture()
  assert.equal(state.form.recording_recovery_enabled, false)
  assert.equal(state.form.recording_recovery_timeout_secs, 120)
  assert.doesNotMatch(html, /aria-label="最长恢复时间"/)
  const enabled = await settingsFixture({ recording_recovery_enabled: true })
  assert.equal(enabled.state.form.recording_recovery_enabled, true)
  assert.match(enabled.html, /aria-label="最长恢复时间"/)
  assert.match(html, /停止恢复.*保留监控/)
  state.form.recording_recovery_enabled = true
  state.form.recording_recovery_timeout_secs = 15
  await state.onSave()
  assert.equal(saved[0].recording_recovery_enabled, true)
  assert.equal(saved[0].recording_recovery_timeout_secs, 15)
  await state.onOpen()
  assert.equal(state.form.recording_recovery_enabled, true)
  assert.equal(state.form.recording_recovery_timeout_secs, 15)
  for (const invalid of [0, 9, 3601, 10.5, undefined]) {
    state.form.recording_recovery_timeout_secs = invalid
    await state.onSave()
  }
  assert.equal(saved.length, 1)
  assert.equal(errors.length, 5)
  const disabled = await settingsFixture({ recording_recovery_enabled: false })
  assert.doesNotMatch(disabled.html, /aria-label="最长恢复时间"/)
  await disabled.state.onSave()
  assert.equal(disabled.saved[0].recording_recovery_enabled, false)
  assert.equal(disabled.saved[0].recording_recovery_timeout_secs, 120)
})

test('disabling reconnect retains its limits and invalid network values cannot save', async () => {
  const { state, html, saved, errors } = await settingsFixture({ ffmpeg_reconnect_enabled: false })
  assert.match(html, /aria-label="网络读写超时"/)
  assert.doesNotMatch(html, /aria-label="重连次数限制"/)
  await state.onSave()
  assert.equal(saved[0].ffmpeg_reconnect_enabled, false)
  assert.equal(saved[0].ffmpeg_reconnect_max_retries, 5)
  for (const [field, max] of [
    ['ffmpeg_rw_timeout_secs', 3600], ['ffmpeg_reconnect_max_retries', 100],
    ['ffmpeg_reconnect_delay_max_secs', 3600], ['ffmpeg_reconnect_delay_total_max_secs', 3600],
  ]) {
    const previous = state.form[field]
    for (const value of [0, max + 1, 1.5, undefined]) {
      state.form[field] = value
      await state.onSave()
    }
    state.form[field] = previous
  }
  assert.equal(saved.length, 1)
  assert.equal(errors.length, 16)
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
  await state.onOpen()
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
  assert.deepEqual(commands, ['get_recording_log_info', 'is_autostart_available', 'get_recording_log_info'])
})
