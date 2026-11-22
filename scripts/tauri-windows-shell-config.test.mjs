import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

const configUrl = new URL('../src-tauri/tauri.windows.conf.json', import.meta.url);
const windowsConfig = JSON.parse(await readFile(configUrl, 'utf8'));

test('Windows config leaves window creation to the shell setup', () => {
  assert.deepEqual(windowsConfig.app?.windows, []);
});
