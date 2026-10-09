import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import ts from 'typescript'

const source = readFileSync(new URL('../src/utils/recovery.ts', import.meta.url), 'utf8')
const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext } }).outputText
const { newerRecovery, recoveryPending, recoveryText } = await import(`data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`)

test('cancelled state rejects late queries and late snapshots, including previous episodes', () => {
  const current = { recovery_id: 5, revision: 10, phase: 'cancelled' }
  assert.equal(newerRecovery(current, { recovery_id: 5, revision: 9, phase: 'starting' }), false)
  assert.equal(newerRecovery(current, current), false)
  assert.equal(newerRecovery(current, { recovery_id: 4, revision: 200 }), false)
  assert.equal(newerRecovery(current, { recovery_id: 6, revision: 1 }), true)
  assert.equal(recoveryPending(current), false)
  assert.equal(recoveryPending({ phase: 'recording' }), false)
})

test('waiting shows bounded countdown while confirmation and restart have distinct labels', () => {
  assert.equal(recoveryText({ phase: 'confirming' }, 0), '正在确认直播状态')
  assert.equal(recoveryText({ phase: 'starting' }, 0), '正在重新录制')
  const status = { phase: 'waiting', attempts: 2, next_attempt_at: '2026-10-09T00:00:05Z' }
  assert.equal(recoveryPending(status), true)
  assert.match(recoveryText(status, Date.parse('2026-10-09T00:00:00Z')), /5 秒.*2 次/)
  assert.match(recoveryText(status, Date.parse('2026-10-09T00:00:06Z')), /0 秒/)
})
