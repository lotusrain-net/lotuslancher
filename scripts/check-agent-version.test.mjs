import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { hasDevelopmentBuildMarker } from './agent-binary.mjs';

test('the production Windows sidecar has no development-only build markers', async () => {
  const sidecar = await readFile(new URL('../src-tauri/binaries/agent-x86_64-pc-windows-msvc.exe', import.meta.url));
  assert.equal(hasDevelopmentBuildMarker(sidecar), false);
});

test('a clean synthetic binary buffer passes the development marker inspection', () => {
  assert.equal(hasDevelopmentBuildMarker(Buffer.from('production agent build')), false);
});

test('development-tagged and development-only binary markers are rejected', () => {
  for (const marker of ['-tags=development', 'development-only', 'verify_browser']) {
    assert.equal(hasDevelopmentBuildMarker(Buffer.from(`Go build info ${marker}`)), true);
  }
});

test('the Windows desktop sidecar uses the desktop release version', async (t) => {
  if (process.platform !== 'win32') {
    t.skip('the bundled sidecar is Windows-only');
    return;
  }

  const packageInfo = JSON.parse(await readFile(new URL('../package.json', import.meta.url), 'utf8'));
  const sidecar = fileURLToPath(new URL('../src-tauri/binaries/agent-x86_64-pc-windows-msvc.exe', import.meta.url));
  const result = spawnSync(sidecar, ['--version'], { encoding: 'utf8', windowsHide: true });

  assert.equal(result.error, undefined);
  assert.equal(result.status, 0);
  assert.equal(result.stdout.trim(), packageInfo.version);
});
