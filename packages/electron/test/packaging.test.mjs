import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtemp, rm, readFile, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath, pathToFileURL } from 'node:url';

test('tarball includes typed runtime/license only and installs without runtime dependencies', async () => {
  const dir = await mkdtemp(resolve(tmpdir(), 'predictor-pack-'));
  const cwd = fileURLToPath(new URL('../', import.meta.url));
  try {
    const [pack] = JSON.parse(execFileSync('npm', ['pack', '--json', '--pack-destination', dir], { cwd, encoding: 'utf8' }));
    assert.deepEqual(pack.files.map(f => f.path).sort(), [
      'LICENSE', 'README.md', 'dist/index.d.ts', 'dist/index.js', 'package.json',
    ]);
    execFileSync('npm', ['install', '--prefix', dir, '--ignore-scripts', '--no-audit', '--no-fund', '--offline', resolve(dir, pack.filename)], { encoding: 'utf8' });
    const installed = resolve(dir, 'node_modules/@linkasu/predictor-electron');
    const metadata = JSON.parse(await readFile(resolve(installed, 'package.json'), 'utf8'));
    assert.equal(metadata.private, true);
    assert.equal(metadata.license, 'GPL-3.0-or-later');
    assert.equal(metadata.dependencies, undefined);
    const api = await import(pathToFileURL(resolve(installed, 'dist/index.js')));
    assert.equal(typeof api.Predictor, 'function');
    assert.equal(typeof api.applySuggestion, 'function');
    // Resolve the public export, not just the file path.
    execFileSync(process.execPath, ['--input-type=module', '-e', "import {Predictor} from '@linkasu/predictor-electron'; if(typeof Predictor !== 'function') process.exit(1)"], { cwd: dir });
    const dts = await readFile(resolve(installed, 'dist/index.d.ts'), 'utf8');
    assert.match(dts, /predict\(input: Input/);
    assert.match(dts, /Promise<Prediction>/);
    assert.match(await readFile(resolve(installed, 'LICENSE'), 'utf8'), /GNU GENERAL PUBLIC LICENSE/);
    await writeFile(resolve(dir, 'consumer.mts'), `
      import { Predictor, applySuggestion, type Input } from '@linkasu/predictor-electron';
      const p = new Predictor({executablePath: '/binary', modelPath: '/model'});
      const input: Input = {text: 'я', caret: 1, language: 'ru'};
      const result = await p.predict(input, {signal: new AbortController().signal});
      const edited: Input = applySuggestion(input, result, 0);
      // @ts-expect-error unsupported language must not typecheck
      const bad: Input = {text: '', caret: 0, language: 'de'};
      // @ts-expect-error prediction is asynchronous
      const sync: string[] = p.predict(input).text;
      void edited; void bad; void sync;
    `);
    execFileSync(process.execPath, [resolve(cwd, 'node_modules/typescript/bin/tsc'),
      '--noEmit', '--strict', '--target', 'ES2022', '--module', 'NodeNext', 'consumer.mts'], { cwd: dir });
  } finally { await rm(dir, { recursive: true }); }
});
