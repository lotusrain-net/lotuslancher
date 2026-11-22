import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import vm from 'node:vm';

const sourceUrl = new URL('../src-tauri/src/shell/bridge.js', import.meta.url);
const source = await readFile(sourceUrl, 'utf8');
const origin = 'https://hub.example.test';

function harness() {
  const calls = [];
  const windowEvents = new Map();
  const documentEvents = new Map();
  const timers = new Map();
  let timerId = 0;
  let fetchReachable = false;
  let reloads = 0;

  const window = {
    __TAURI_INTERNALS__: {
      invoke(command) {
        calls.push(command);
        return Promise.resolve({});
      },
    },
    addEventListener(name, callback) {
      windowEvents.set(name, callback);
    },
  };
  window.top = window;

  const document = {
    readyState: 'loading',
    addEventListener(name, callback) {
      documentEvents.set(name, callback);
    },
  };
  const location = {
    origin,
    reload() { reloads += 1; },
  };

  const context = {
    window,
    document,
    location,
    Promise,
    AbortController,
    setTimeout(callback, delay) {
      const id = ++timerId;
      timers.set(id, { callback, delay });
      return id;
    },
    clearTimeout(id) { timers.delete(id); },
    setInterval() { return 0; },
    fetch() {
      return fetchReachable
        ? Promise.resolve({ status: 200 })
        : Promise.reject(new Error('offline'));
    },
  };

  vm.runInNewContext(source.replace('__LOTUS_CONTROL_ORIGIN__', JSON.stringify(origin)), context);
  documentEvents.get('DOMContentLoaded')();

  async function fireTimer(delay) {
    const entry = [...timers.entries()].find(([, timer]) => timer.delay === delay);
    assert.ok(entry, `timer with ${delay} ms delay was not scheduled`);
    timers.delete(entry[0]);
    entry[1].callback();
    await Promise.resolve();
    await Promise.resolve();
  }

  return {
    calls,
    async dispatch(name) {
      windowEvents.get(name)();
      await Promise.resolve();
    },
    fireTimer,
    setFetchReachable(value) { fetchReachable = value; },
    get reloads() { return reloads; },
  };
}

test('offline is reported only after the debounced reachability probe fails', async () => {
  const app = harness();
  app.calls.length = 0;

  await app.dispatch('offline');
  assert.equal(app.calls.includes('desktop_hub_offline'), false);

  await app.fireTimer(2000);
  assert.equal(app.calls.includes('desktop_hub_offline'), true);

  await app.fireTimer(5000);
  await app.fireTimer(2000);
  assert.equal(app.calls.filter((command) => command === 'desktop_hub_offline').length, 1);
});

test('a reachable Hub reloads after an offline transition and reports ready', async () => {
  const app = harness();
  app.calls.length = 0;

  await app.dispatch('offline');
  await app.fireTimer(2000);
  assert.equal(app.calls.includes('desktop_hub_offline'), true);

  app.setFetchReachable(true);
  await app.dispatch('online');
  await app.fireTimer(2000);
  assert.equal(app.reloads, 1);
});
