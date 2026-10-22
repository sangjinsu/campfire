import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import vm from 'node:vm';

const root = new URL('../../', import.meta.url);
const source = await readFile(new URL('crates/campfire-server/src/dashboard/assets/app.js', root), 'utf8');
const markup = await readFile(new URL('crates/campfire-server/src/dashboard/assets/index.html', root), 'utf8');

// A deliberately small DOM harness exercises async control flow, not browser layout.
class Element {
  constructor(tag = 'div') {
    this.tagName = tag; this.children = []; this.listeners = new Map(); this.attributes = {};
    this.value = ''; this.textContent = ''; this.hidden = false; this.disabled = false; this.checked = true; this.open = false;
    this.classList = { toggle() {} };
  }
  addEventListener(event, handler) { this.listeners.set(event, handler); }
  emit(event) { return this.listeners.get(event)?.({ preventDefault() {} }); }
  setAttribute(name, value) { this.attributes[name] = value; }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = children; this.textContent = ''; }
  focus() { this.focused = true; }
  showModal() { this.open = true; }
  close() { this.open = false; this.emit('close'); }
}

function harness() {
  const elements = new Map([...markup.matchAll(/id="([^"]+)"/g)].map((match) => [match[1], new Element()]));
  for (const id of ['dashboard', 'toolbar', 'notice']) elements.get(id).hidden = true;
  elements.get('visibility-filter').value = 'all';
  const document = new Element();
  document.hidden = false;
  document.getElementById = (id) => { assert(elements.has(id), `Missing element ${id}`); return elements.get(id); };
  document.createElement = (tag) => new Element(tag);
  document.createElementNS = (_, tag) => new Element(tag);
  const window = new Element();
  const timers = new Map();
  let timerId = 0;
  window.setTimeout = (handler, delay) => { timers.set(++timerId, { handler, delay }); return timerId; };
  window.clearTimeout = (id) => timers.delete(id);
  let intervalHandler = null;
  window.setInterval = (handler) => { intervalHandler = handler; return 1; };
  let clockOffset = 0;
  class TestDate extends Date { static now() { return Date.now() + clockOffset; } }
  const requests = [];
  function fetch(url, options) {
    let resolve;
    const promise = new Promise((done) => { resolve = done; });
    requests.push({ url, options, resolve: (body, status = 200) => resolve({ status, ok: status >= 200 && status < 300, json: async () => body }) });
    return promise;
  }
  const context = vm.createContext({ document, window, fetch, Intl, Date: TestDate, Map, URLSearchParams, AbortController, DOMException,
    Option: function (label, value) { const option = new Element('option'); option.textContent = label; option.value = value; return option; },
  });
  vm.runInContext(source, context);
  return { elements, document, window, timers, requests, $: (id) => elements.get(id), advance: (ms) => { clockOffset += ms; }, tick: () => intervalHandler?.() };
}

const flush = () => new Promise((resolve) => setImmediate(resolve));
const envelope = (data, seq = 1, boot = 'boot-a', state = 'ready') => ({ schema_version: 1, boot_id: boot, sample_seq: seq, collected_at: 1000000 + seq * 1000, cache_age_ms: 0, state, data });
const statusData = (overrides = {}) => ({
  uptime_ms: 5000, stopping: false,
  metrics: { active_connections: 8, active_sessions: 9, unauthenticated_connections: 0, active_rooms: 1, inputs_processed: 50, snapshots: 30 },
  rates: { inputs_per_second: 10, snapshots_per_second: 6 },
  rooms: { total: 1, public: 1, private: 0, connected_members: 8, reserved_members: 1 },
  games: [{ game_id: 'forest', total: 1 }],
  process: { pid: 123, state: 'ready', cpu_percent: 0, rss_bytes: 10485760, age_ms: 0 }, ...overrides,
});
const sample = (seq = 1, overrides = {}) => ({ sample_seq: seq, collected_at: 1000000 + seq * 1000, interval_ms: 1000, ...statusData(), ...overrides });
const room = { game_id: 'forest', room_id: '<room & id>', room_instance_id: 'instance-a', public: true, capacity: 16, connected_members: 8, reserved_members: 1, server_tick: 50, snapshot_seq: 30, summary_age_ms: 0, state: 'ready' };
async function login(h) {
  h.$('admin-key').value = 'test-fixture-key-never-rendered';
  h.$('login-form').emit('submit');
  assert.equal(h.$('admin-key').value, '');
  await flush();
}
async function completeBatch(h, offset = 0, { seq = 1, boot = 'boot-a', data = statusData(), rooms = [room], next = null, samples = [sample(seq)], reset = true } = {}) {
  h.requests[offset].resolve(envelope(data, seq, boot));
  await flush();
  assert.equal(h.requests.length, offset + 3);
  h.requests[offset + 1].resolve(envelope({ rooms, next_cursor: next, total_filtered: rooms.length }, seq, boot));
  h.requests[offset + 2].resolve(envelope({ samples, reset, oldest_seq: samples[0]?.sample_seq ?? null }, seq, boot));
  await flush();
}

{
  const h = harness(); await login(h);
  h.requests[0].resolve(envelope(null, 0, 'boot-a', 'warming_up')); await flush();
  assert.equal(h.$('dashboard').hidden, false, '200 warming_up authenticates before room/history responses');
  assert.match(h.$('notice').textContent, /첫 관측 표본/);
  h.requests[1].resolve(envelope(null, 0, 'boot-a', 'warming_up'));
  h.requests[2].resolve(envelope({ samples: [], reset: true, oldest_seq: null }, 0, 'boot-a', 'warming_up')); await flush();
  assert.equal(h.$('metric-cpu').textContent, '—');
}
{
  const h = harness(); await login(h); await completeBatch(h);
  assert.equal(h.$('metric-cpu').textContent, '0', 'CPU zero is a measured value');
  assert.equal(h.$('metric-rss').textContent, '10', 'RSS bytes become MiB');
  assert.equal(h.$('metric-connections').textContent, '8');
  assert.equal(h.$('room-rows').children[0].children[0].children[1].textContent, room.room_id, 'Room IDs stay text');
  h.$('refresh').emit('click'); await flush();
  h.$('logout').emit('click');
  assert.equal(h.requests[3].options.signal.aborted, true);
  h.requests[3].resolve(envelope(statusData(), 2)); await flush();
  assert.equal(h.$('dashboard').hidden, true);
  assert.equal(h.$('metric-connections').textContent, '—', 'Late status cannot restore logged out data');
  assert.equal(h.$('room-rows').children.length, 0);
  assert.equal(h.$('chart-connections').children.length, 0);
  assert.equal(h.timers.size, 0);
}
{
  const h = harness(); await login(h); h.requests[0].resolve(envelope(statusData())); await flush();
  h.requests[1].resolve({ error: { code: 'unauthorized', message: 'ignored' } }, 401); await flush();
  assert.equal(h.$('dashboard').hidden, true);
  assert.match(h.$('login-error').textContent, /유효/);
  assert.equal(h.$('login-error').textContent.includes('test-fixture-key'), false);
  h.requests[2].resolve(envelope({ samples: [sample()], reset: true })); await flush();
  assert.equal(h.$('chart-connections').children.length, 0, 'Late history after 401 stays discarded');
  assert.equal(h.$('room-rows').children.length, 0);
}
{
  const h = harness(); await login(h);
  h.$('room-query').value = 'second'; h.$('filters').emit('submit');
  assert.equal(h.requests.length, 1, 'Replacement batch waits for canceled batch to settle');
  h.requests[0].resolve(envelope(statusData())); await flush();
  assert.equal(h.requests.length, 2, 'Only replacement status starts');
  await completeBatch(h, 1);
  assert.equal(new URL(h.requests[2].url, 'http://localhost').searchParams.get('q'), 'second');
  assert.equal(h.requests[0].options.signal.aborted, true);
}
{
  const h = harness(); await login(h); h.requests[0].resolve(envelope(statusData())); await flush();
  h.$('auto-refresh').checked = false; h.$('auto-refresh').emit('change');
  assert.equal(h.requests[1].options.signal.aborted, false, 'OFF lets the current batch complete');
  h.requests[1].resolve(envelope({ rooms: [room], next_cursor: null, total_filtered: 1 }));
  h.requests[2].resolve(envelope({ samples: [sample()], reset: true })); await flush();
  assert.equal(h.$('metric-connections').textContent, '8');
  assert.equal(h.timers.size, 0, 'OFF schedules no future poll');
  assert.equal(h.$('connection-state').textContent, '자동 갱신 꺼짐');
  h.advance(11000); h.tick();
  assert.equal(h.$('connection-state').textContent, '자동 갱신 꺼짐', 'Intentional pause stays distinct from failure after 11 seconds');
}
{
  const h = harness(); await login(h); await completeBatch(h);
  h.$('refresh').emit('click'); await flush();
  h.document.hidden = true; h.document.emit('visibilitychange');
  assert.equal(h.requests[3].options.signal.aborted, true);
  assert.equal(h.timers.size, 0);
  h.requests[3].resolve(envelope(statusData({ metrics: { active_connections: 99 } }), 2)); await flush();
  assert.equal(h.$('metric-connections').textContent, '8', 'Hidden late response cannot replace last displayed sample');
  h.document.hidden = false; h.document.emit('visibilitychange'); await flush();
  assert.equal(h.requests.length, 5, 'Visible auto ON immediately refreshes');
  await completeBatch(h, 4, { seq: 3 });
}
{
  const h = harness(); await login(h); await completeBatch(h, 0, { next: 'opaque-page-two' });
  h.$('next-page').emit('click'); await completeBatch(h, 3, { seq: 2, samples: [sample(2)], reset: false });
  assert.equal(new URL(h.requests[4].url, 'http://localhost').searchParams.get('cursor'), 'opaque-page-two');
  assert.equal(new URL(h.requests[5].url, 'http://localhost').searchParams.get('after_seq'), '1');
  h.$('refresh').emit('click'); await completeBatch(h, 6, { seq: 1, boot: 'boot-b', samples: [sample()] });
  assert.equal(new URL(h.requests[7].url, 'http://localhost').searchParams.has('cursor'), false, 'Boot change resets room page');
  assert.equal(new URL(h.requests[8].url, 'http://localhost').searchParams.has('after_seq'), false, 'Boot change requests full history');
  assert.match(h.$('page-description').textContent, /^1페이지/);
  assert.match(h.$('history-description').textContent, /^1개 표본/);
}
{
  const h = harness(); await login(h);
  const later = sample(5, { process: { cpu_percent: 125, rss_bytes: null } });
  await completeBatch(h, 0, { data: statusData({ process: { state: 'warming_up', cpu_percent: null, rss_bytes: null } }), samples: [sample(1), later] });
  assert.equal(h.$('metric-cpu').textContent, '—', 'Null CPU stays unmeasured');
  assert.equal(h.$('chart-cpu-value').textContent, '125%', 'CPU above 100 is preserved');
  const paths = h.$('chart-connections').children.filter((element) => element.tagName === 'path');
  assert.equal((paths[0].attributes.d.match(/M/g) || []).length, 2, 'Missing intervals create separate chart segments');
}
{
  const h = harness(); await login(h); await completeBatch(h);
  h.window.emit('pagehide');
  assert.equal(h.timers.size, 0, 'pagehide stops future polling');
  assert.equal(h.$('connection-state').textContent, '화면 비활성 · 일시중단');
  h.$('auto-refresh').checked = false;
  h.window.emit('pageshow'); await flush();
  assert.equal(h.requests.length, 3, 'pageshow with auto OFF does not refresh');
  h.$('auto-refresh').checked = true; h.$('auto-refresh').emit('change'); await flush();
  assert.equal(h.requests.length, 4, 'Enabling auto refresh resumes immediately');
  await completeBatch(h, 3, { seq: 2 });
}
{
  const h = harness(); await login(h);
  const samples = Array.from({ length: 11 }, (_, index) => sample(index + 1));
  await completeBatch(h, 0, { seq: 11, samples });
  const chart = h.$('chart-connections');
  const path = chart.children.find((element) => element.tagName === 'path').attributes.d;
  const coordinates = [...path.matchAll(/[ML]([\d.]+),/g)].map((match) => Number(match[1]));
  assert(coordinates.at(-1) - coordinates[0] >= 400, 'First 11 samples span the chart instead of a vertical line at the right edge');
  const format = new Intl.DateTimeFormat('ko-KR', { hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false });
  const labels = chart.children.filter((element) => element.tagName === 'text').map((element) => element.textContent);
  assert(labels.includes(format.format(samples[0].collected_at)), 'Axis names the actual start time');
  assert(labels.includes(format.format(samples.at(-1).collected_at)), 'Axis names the actual latest sample time');
  assert.match(h.$('history-description').textContent, /관측 10초/, 'Observed duration is distinct from the 15 minute retention limit');
}
{
  const h = harness(); await login(h); await completeBatch(h);
  const chart = h.$('chart-connections');
  const points = chart.children.filter((element) => element.tagName === 'circle');
  const path = chart.children.find((element) => element.tagName === 'path').attributes.d;
  assert.equal(points.length, 1, 'One observed sample stays visible as one point');
  assert.equal(path.includes('L'), false, 'One sample does not imply a measured interval');
  assert.match(chart.attributes['aria-label'], /1개 표본/, 'Chart accessible name states the actual number of samples');
  assert.match(h.$('history-description').textContent, /관측 시작/, 'One sample does not claim an observed time range');
}
{
  const h = harness(); await login(h);
  await completeBatch(h, 0, { seq: 3, samples: [
    sample(1, { process: { cpu_percent: 0, rss_bytes: 0 } }),
    sample(2, { process: { cpu_percent: null, rss_bytes: null } }),
    sample(3, { process: { cpu_percent: 125, rss_bytes: 10485760 } }),
  ] });
  const chart = h.$('chart-cpu');
  const path = chart.children.find((element) => element.tagName === 'path').attributes.d;
  assert.equal((path.match(/M/g) || []).length, 2, 'Null measurements break otherwise adjacent samples');
  assert.equal(path.includes('L'), false, 'Null is not joined across or plotted as zero');
  assert.equal(h.$('chart-cpu-value').textContent, '125%', 'Adaptive chart preserves multicore CPU above 100 percent');
}
{
  const h = harness(); await login(h);
  await completeBatch(h, 0, { samples: [] });
  assert.equal(h.$('chart-connections').children.filter((element) => element.tagName === 'path').length, 0, 'Empty history never creates a measured line');
  assert.match(h.$('chart-connections-summary').textContent, /수집 대기/);
  h.$('refresh').emit('click');
  await completeBatch(h, 3, { seq: 901, samples: [sample(1), sample(901)] });
  assert.match(h.$('history-description').textContent, /관측 15분 0초/, '15 minute boundary reports the observed time accurately');
  const path = h.$('chart-connections').children.find((element) => element.tagName === 'path').attributes.d;
  const coordinates = [...path.matchAll(/[ML]([\d.]+),/g)].map((match) => Number(match[1]));
  assert(coordinates.at(-1) - coordinates[0] >= 400, '15 minute retained history spans the same readable chart');
}
{
  const h = harness(); await login(h); await completeBatch(h, 0, { next: 'page-two' });
  const scroll = h.$('room-table-scroll');
  scroll.scrollTop = 320;
  h.document.activeElement = h.$('room-rows').children[0].children[0].children[1];
  h.$('refresh').emit('click'); await completeBatch(h, 3, { seq: 2, next: 'page-two' });
  assert.equal(scroll.scrollTop, 320, 'Polling keeps the current table scroll position');
  assert.equal(h.$('room-rows').children[0].children[0].children[1].focused, true, 'Polling restores keyboard focus to the same room identity');
  h.document.activeElement = null;
  h.$('next-page').emit('click');
  assert.equal(scroll.scrollTop, 0, 'A user page change returns to the top row');
  await completeBatch(h, 6, { seq: 3 });
  scroll.scrollTop = 240;
  h.$('room-query').value = 'new filter'; h.$('filters').emit('submit');
  assert.equal(scroll.scrollTop, 0, 'Applying a filter returns to the top row');
  await completeBatch(h, 9, { seq: 4 });
}
console.log('Dashboard UI async controls: 14 scenarios passed (existing 9 controls plus initial range, single sample, null segments, empty/15 minute range, room scroll/focus).');
