import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import ts from 'typescript'

// Compile the same pure presentation helper used by TaskList, without a browser.
const source = readFileSync(new URL('../src/utils/recordingRows.ts', import.meta.url), 'utf8')
const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext } }).outputText
const { recordingRows } = await import(`data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`)

function session(overrides = {}) {
  return { id: 42, room_id: 1, status: 'recording', start_time: '2026-09-25 00:00:00', trigger: 'auto',
    file_path: null, segment_output: { duration_secs: 3600 }, segments: [], ...overrides }
}
function segment(index, overrides = {}) {
  return { id: 100 + index, task_id: 42, segment_index: index, file_path: `anchor_part00${index}.flv`,
    status: 'completed', conversion_state: 'idle', deleted: false, conversion_error: null,
    start_time: `2026-09-25 0${index}:00:00`, ...overrides }
}

test('each segment is a row and stopping the active segment still addresses the parent session', () => {
  const task = session({ segments: [segment(1, { conversion_state: 'converting' }), segment(2, { status: 'recording' })] })
  const rows = recordingRows([task])
  assert.equal(rows.length, 2)
  assert.equal(rows[0].segmentIndex, 2)
  assert.equal(rows[0].id, 42)
  assert.equal(rows[0].segmentId, 102)
  assert.equal(rows[0].status, 'recording')
  assert.equal(rows[1].status, 'finalizing')
  assert.equal(rows[1].conversionState, 'converting')
  assert.equal(task.status, 'recording')
})

test('deleted segments stay hidden without showing an extra parent row', () => {
  assert.deepEqual(recordingRows([session({ segments: [segment(1, { deleted: true })] })]), [])
})

test('old single-file history and start failures remain accessible', () => {
  const legacy = session({ id: 8, segment_output: null, status: 'completed', file_path: 'old.flv' })
  const failed = session({ id: 9, status: 'failed' })
  const rows = recordingRows([legacy, failed])
  assert.equal(rows.length, 2)
  assert.equal(rows[0].rowKey, 'task-9')
  assert.equal(rows[1].file_path, 'old.flv')
  assert.equal(rows[1].segmentId, undefined)
})

test('closing a session disables the last segment stop button but retains completed and failed conversions', () => {
  const rows = recordingRows([session({ status: 'finalizing', segments: [
    segment(1, { conversion_state: 'failed', conversion_error: 'failure' }), segment(2, { status: 'recording' }),
  ] })])
  assert.equal(rows[0].status, 'finalizing')
  assert.equal(rows[1].status, 'completed')
  assert.equal(rows[1].conversionError, 'failure')
  assert.equal(rows[1].file_path, 'anchor_part001.flv')
})
