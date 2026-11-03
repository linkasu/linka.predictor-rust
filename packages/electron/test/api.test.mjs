import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm, access } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';
import { Predictor, PredictorError, applySuggestion } from '../dist/index.js';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const fixture = fileURLToPath(new URL('./sidecar.mjs', import.meta.url));
const binary = process.env.LP_ELECTRON_BIN ?? resolve(root, 'target/debug/lp-electron');
const model = resolve(root, 'artifacts/tiny/runtime-v1.lpk');
const input = { text: 'я хочу ', caret: 7, language: 'ru' };
const error = code => e => e instanceof PredictorError && e.code === code;
const fake = (mode, timeoutMs = 2000) => new Predictor({ executablePath: fixture, modelPath: `/fixture/${mode}`, timeoutMs });

test('real Rust lifecycle, JSON, prefix-only caret and immutable snapshot', async () => {
  await access(binary);
  const p = new Predictor({ executablePath: binary, modelPath: model });
  assert.equal(p.state, 'idle');
  await p.start();
  assert.equal(p.state, 'ready');
  const end = await p.predict(input);
  assert.ok(end.text.length > 0);
  const middle = { ...input, text: input.text + 'завтра😀' };
  const pending = p.predict(middle);
  middle.text = 'changed after submission';
  const result = await pending;
  assert.deepEqual(result.text, end.text);
  assert.equal(result.pos, end.pos);
  const applied = applySuggestion(result.input, result, 0);
  assert.ok(applied.text.endsWith('завтра😀'));
  assert.equal(applied.text.slice(applied.caret), 'завтра😀');
  for (const prefix of ['хо', 'хчоу', '😀 хо']) {
    const before = await p.predict({ text: prefix, caret: prefix.length, language: 'ru' });
    assert.ok(before.text.length > 0);
    assert.ok(before.pos < 0);
    const inside = { text: prefix + ' суффикс', caret: prefix.length, language: 'ru' };
    const prediction = await p.predict(inside);
    const edit = applySuggestion(inside, prediction, 0);
    assert.equal(edit.text, applySuggestion(before.input, before, 0).text + ' суффикс');
    assert.equal(edit.text.slice(edit.caret), ' суффикс');
  }
  await p.dispose();
  await p.dispose();
  assert.equal(p.state, 'disposed');
  await assert.rejects(p.predict(input), error('DISPOSED'));
  await assert.rejects(p.start(), error('DISPOSED'));
});

test('missing/corrupt models fail closed; retry can recover; EN is manual', async () => {
  const dir = await mkdtemp(resolve(tmpdir(), 'predictor-model-'));
  const path = resolve(dir, 'model.lpk');
  const p = new Predictor({ executablePath: binary, modelPath: path });
  try {
    await assert.rejects(p.start(), error('MODEL_MISSING'));
    assert.equal(p.state, 'failed');
    await writeFile(path, 'not an lpk model');
    await assert.rejects(p.predict(input), error('MODEL_CORRUPT'));
    const en = await p.predict({ text: 'hello', caret: 2, language: 'en' });
    assert.deepEqual(en.text, []);
    assert.equal(p.state, 'ready');
    const { copyFile } = await import('node:fs/promises');
    await copyFile(model, path);
    assert.ok((await p.predict(input)).text.length > 0);
  } finally { await p.dispose(); await rm(dir, { recursive: true }); }
});

test('UTF16 replacement, append, suffix, stale caret and invalid selection', () => {
  const current = { text: '😀првет SUFFIX', caret: 7, language: 'ru' };
  const result = { input: current, text: ['привет'], pos: -5, endOfWord: true };
  assert.deepEqual(applySuggestion(current, result, 0), { ...current, text: '😀привет SUFFIX', caret: 8 });
  const append = { ...result, pos: 0, text: ['!'] };
  assert.equal(applySuggestion(current, append, 0).text, '😀првет! SUFFIX');
  assert.throws(() => applySuggestion({ ...current, caret: 6 }, result, 0), error('STALE'));
  assert.throws(() => applySuggestion(current, result, 99), error('INVALID_INPUT'));
  const emoji = { text: '😀tail', caret: 2, language: 'ru' };
  assert.throws(() => applySuggestion(emoji, { ...result, input: emoji, pos: -1 }, 0), error('INVALID_INPUT'));
});

test('untrusted input bounds and surrogate boundaries', async () => {
  const p = fake('good');
  for (const bad of [null, { ...input, caret: -1 }, { ...input, caret: 0.5 },
    { ...input, language: 'de' }, { ...input, text: 'x'.repeat(16385) },
    { text: '😀', caret: 1, language: 'ru' }, { text: '\uD800', caret: 1, language: 'ru' }]) {
    await assert.rejects(p.predict(bad), error('INVALID_INPUT'));
  }
  assert.throws(() => new Predictor({ executablePath: 'relative', modelPath: '/model' }), error('INVALID_INPUT'));
  await p.dispose();
});

test('stdin not argv; structured errors for spawn/crash/protocol/output bound', async () => {
  const p = fake('good');
  assert.deepEqual((await p.predict(input)).text, [input.text]);
  await p.dispose();
  for (const [mode, code] of [['crash', 'PROCESS'], ['malformed', 'PROTOCOL'], ['invalid-utf8', 'PROTOCOL'], ['overflow', 'PROTOCOL'], ['bad-pos', 'PROTOCOL']]) {
    const failing = fake(mode);
    await assert.rejects(failing.predict(input), error(code));
    await failing.dispose();
  }
  const split = fake('split-surrogate');
  await assert.rejects(split.predict({ text: '😀', caret: 2, language: 'ru' }), error('PROTOCOL'));
  await split.dispose();
  const missing = new Predictor({ executablePath: '/no/such/lp-electron', modelPath: model });
  await assert.rejects(missing.start(), error('SPAWN'));
  await missing.dispose();
});

test('RU UTF8 prefix bound is checked before spawning; EN remains manual', async () => {
  const p = new Predictor({ executablePath: '/no/such/lp-electron', modelPath: model });
  try {
    for (const text of ['я'.repeat(8192) + 'я', '😀'.repeat(4096) + '😀']) {
      const state = p.state;
      await assert.rejects(p.predict({ text, caret: text.length, language: 'ru' }), error('INVALID_INPUT'));
      assert.equal(p.state, state);
      assert.deepEqual((await p.predict({ text, caret: text.length, language: 'en' })).text, []);
    }
    for (const text of ['я'.repeat(8192), '😀'.repeat(4096)]) {
      await assert.rejects(p.predict({ text, caret: text.length, language: 'ru' }), error('SPAWN'));
    }
  } finally { await p.dispose(); }
  const echo = fake('good');
  try {
    const prefix = 'я'.repeat(8192);
    const result = await echo.predict({ text: prefix + 'suffix', caret: prefix.length, language: 'ru' });
    assert.deepEqual(result.text, [prefix]);
  } finally { await echo.dispose(); }
});

test('cancel, stale queued requests, newest wins, disposal during inference', async () => {
  const p = fake('slow');
  const first = p.predict(input);
  const firstRejected = assert.rejects(first, error('STALE'));
  const secondRejected = assert.rejects(p.predict(input), error('STALE'));
  const latest = p.predict({ ...input, text: 'новый', caret: 5 });
  await Promise.all([firstRejected, secondRejected]);
  assert.deepEqual((await latest).text, ['новый']);
  const controller = new AbortController();
  const cancelled = assert.rejects(p.predict(input, { signal: controller.signal }), error('CANCELLED'));
  controller.abort();
  await cancelled;
  const running = assert.rejects(p.predict(input), error('DISPOSED'));
  await p.dispose();
  await running;
});

test('active child cancellation, timeout and forced shutdown leave no child', async () => {
  // Use the actual child spawn event to cancel after spawn, not a timed guess.
  const { once } = await import('node:events');
  const { channel } = await import('node:diagnostics_channel');
  for (const action of ['abort', 'dispose', 'replace', 'timeout']) {
    const p = fake('stubborn', 500);
    const controller = new AbortController();
    const spawned = new Promise(resolve => {
      const listener = ({ process: child }) => { channel('child_process').unsubscribe(listener); resolve(child); };
      channel('child_process').subscribe(listener);
    });
    const expected = { abort: 'CANCELLED', dispose: 'DISPOSED', replace: 'STALE', timeout: 'TIMEOUT' }[action];
    const rejected = assert.rejects(p.predict(input, { signal: controller.signal }), error(expected));
    const child = await spawned;
    const closed = once(child, 'close');
    if (action === 'abort') controller.abort();
    if (action === 'dispose') await p.dispose();
    if (action === 'replace') await p.predict({ ...input, language: 'en' });
    await rejected;
    await closed;
    assert.throws(() => process.kill(child.pid, 0), { code: 'ESRCH' });
    await p.dispose();
  }
});

test('burst is latest-only with at most one live sidecar', async () => {
  const { channel } = await import('node:diagnostics_channel');
  const p = fake('slow');
  let live = 0;
  let peak = 0;
  let spawns = 0;
  let firstSpawn;
  const started = new Promise(resolve => { firstSpawn = resolve; });
  const listener = ({ process: child }) => {
    spawns++;
    peak = Math.max(peak, ++live);
    child.once('close', () => { live--; });
    firstSpawn();
  };
  channel('child_process').subscribe(listener);
  try {
    const first = assert.rejects(p.predict(input), error('STALE'));
    await started;
    const superseded = Array.from({ length: 50 }, () => assert.rejects(p.predict(input), error('STALE')));
    const newest = p.predict({ text: 'last', caret: 4, language: 'ru' });
    await Promise.all([first, ...superseded]);
    assert.deepEqual((await newest).text, ['last']);
    assert.equal(peak, 1);
    assert.equal(spawns, 2);
    assert.equal(live, 0);
  } finally {
    await p.dispose();
    channel('child_process').unsubscribe(listener);
  }
});
