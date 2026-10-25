(() => {
  // Test-only controlled response timing in the actual gstack browser page.
  // The wrapper records URLs and counts only, never request headers or keys.
  const original = window.fetch.bind(window);
  const qa = { mode: 'normal', pending: [], calls: [], aborts: 0 };
  qa.release = () => { for (const release of qa.pending.splice(0)) release(); };
  qa.hide = () => {
    Object.defineProperty(document, 'hidden', { configurable: true, value: true });
    document.dispatchEvent(new Event('visibilitychange'));
  };
  qa.show = () => {
    Object.defineProperty(document, 'hidden', { configurable: true, value: false });
    document.dispatchEvent(new Event('visibilitychange'));
  };
  qa.verifyChartDiagnostics = (viewport = 'desktop') => {
    const byId = (id) => document.getElementById(id);
    const check = (condition, message) => { if (!condition) throw new Error(`Dashboard chart: ${message}`); };
    byId('range-15m').click();
    check(byId('range-15m').getAttribute('aria-pressed') === 'true', '15 minute range');
    check(byId('chart-connections').dataset.sampleCount === '63', 'real retained samples');
    byId('range-1m').click();
    check(byId('chart-connections').dataset.sampleCount === '61', 'one minute samples');
    byId('range-5m').click();
    check(byId('chart-connections').dataset.sampleCount === '62', 'five minute samples');
    byId('range-1m').click();
    const chart = byId('chart-connections');
    const width = chart.getBoundingClientRect().width;
    check(viewport === 'mobile' ? width < 480 : width > 480, `${viewport} SVG geometry`);
    const matrix = chart.getScreenCTM();
    check(Boolean(matrix), 'rendered SVG screen transform');
    const path = chart.querySelector('path.chart-line');
    const points = [...path.getAttribute('d').matchAll(/[ML]([\d.]+),([\d.]+)/g)].map((match) => ({ x: Number(match[1]), y: Number(match[2]) }));
    check(points.length === 61, 'all one minute points in SVG path');
    const screenPoint = (index) => new DOMPoint(points[index].x, points[index].y).matrixTransform(chart.getScreenCTM());
    const firstPoint = screenPoint(0);
    const left = chart.getBoundingClientRect().left;
    check(viewport === 'mobile' ? firstPoint.x - left < 58 : firstPoint.x - left > 68, `${viewport} viewBox transform`);
    const pointerAt = (index) => {
      const point = screenPoint(index);
      chart.dispatchEvent(new PointerEvent('pointermove', { bubbles: true, clientX: point.x, clientY: point.y }));
    };
    byId('range-15m').click();
    check(byId('delta-admission_rejections').textContent === '—', 'counter reset is unknown');
    check(byId('delta-abusive_frames').textContent === '3', 'counter increase');
    check(byId('delta-total').textContent === '—', 'unknown total');
    byId('range-1m').click();
    chart.focus();
    check(document.activeElement === chart, 'keyboard focus');
    chart.dispatchEvent(new KeyboardEvent('keydown', { key: 'Home', bubbles: true }));
    const first = byId('chart-readout').textContent;
    check(first.includes('표본 841'), 'Home selects first one minute point');
    chart.dispatchEvent(new KeyboardEvent('keydown', { key: 'End', bubbles: true }));
    check(byId('chart-readout').textContent.includes('표본 901'), 'End selects last real point');
    pointerAt(0);
    check(byId('chart-readout').textContent === first, 'pointer on drawn first point matches keyboard');
    for (let index = 0; index < 30; index += 1) chart.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
    const middle = byId('chart-readout').textContent;
    check(middle.includes('표본 871'), 'keyboard middle point');
    chart.dispatchEvent(new KeyboardEvent('keydown', { key: 'End', bubbles: true }));
    pointerAt(30);
    check(byId('chart-readout').textContent === middle, 'pointer on drawn middle point matches keyboard');
    byId('range-15m').click();
    const cpu = byId('chart-cpu');
    cpu.focus();
    cpu.dispatchEvent(new KeyboardEvent('keydown', { key: 'Home', bubbles: true }));
    check(byId('chart-readout').textContent.includes('CPU 0%'), 'measured zero');
    cpu.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
    check(byId('chart-readout').textContent.includes('CPU 측정값 없음'), 'null remains unknown');
    return { viewport, svgWidth: Math.round(width), ranges: [61, 62, 63], keyboardFocus: true, pointerFirst: true, pointerMiddle: true, counterReset: true, nullVsZero: true };
  };
  window.fetch = async (url, options) => {
    qa.calls.push(String(url));
    if (options.signal) options.signal.addEventListener('abort', () => { qa.aborts += 1; }, { once: true });
    if (qa.mode === 'unauthorized') {
      qa.mode = 'normal';
      return new Response('{}', { status: 401, headers: { 'Content-Type': 'application/json' } });
    }
    if (qa.mode === 'warming') {
      const data = String(url).includes('/history') ? { samples: [], reset: true, oldest_seq: null }
        : String(url).includes('/rooms') ? { rooms: [], next_cursor: null, total_filtered: 0 } : null;
      return new Response(JSON.stringify({ schema_version: 1, boot_id: 'controlled-warming', state: 'warming_up', sample_seq: 0, collected_at: null, cache_age_ms: null, data }), { status: 200, headers: { 'Content-Type': 'application/json' } });
    }
    const response = await original(url, options);
    if (['cpu-zero', 'cpu-null'].includes(qa.mode) && String(url).includes('/status')) {
      const body = await response.json();
      body.data.process.cpu_percent = qa.mode === 'cpu-zero' ? 0 : null;
      body.data.process.state = qa.mode === 'cpu-zero' ? 'ready' : 'warming_up';
      return new Response(JSON.stringify(body), { status: response.status, headers: { 'Content-Type': 'application/json' } });
    }
    if (qa.mode === 'delay' && String(url).includes('/status')) {
      qa.mode = 'normal';
      // Deliberately ignore abort after response arrival to exercise generation guards.
      await new Promise((resolve) => qa.pending.push(resolve));
    }
    if (qa.mode === 'chart-diagnostics' && String(url).includes('/history') && response.ok) {
      const body = await response.json();
      const end = Date.now();
      const counter = (admission, frames) => ({ admission_rejections: admission, abusive_frames: frames,
        abusive_inputs: 0, shared_queue_overloads: 0, reliable_queue_disconnects: 0,
        write_deadline_disconnects: 0, room_panics: 0, commands_failed: 0 });
      const makeSample = (sample_seq, collected_at, connections, cpu, rss, admission, frames) => ({
        sample_seq, collected_at, metrics: { active_connections: connections, ...counter(admission, frames) },
        rates: { inputs_per_second: connections, snapshots_per_second: connections },
        process: { cpu_percent: cpu, rss_bytes: rss },
      });
      const dense = Array.from({ length: 61 }, (_, index) => makeSample(
        841 + index, end - (60 - index) * 1000, 3 + index, 4 + index, 1048576 + index * 1024,
        index === 60 ? 4 : 3, index === 60 ? 3 : 2,
      ));
      body.data = { reset: true, oldest_seq: 1, samples: [
        makeSample(1, end - 900000, 0, 0, 0, 10, 0),
        makeSample(301, end - 300000, 2, null, null, 2, 1),
        ...dense,
      ] };
      return new Response(JSON.stringify(body), { status: response.status, headers: { 'Content-Type': 'application/json' } });
    }
    return response;
  };
  window.__dashboardQA = qa;
  return 'Controlled race/visibility fixture installed; headers are never retained';
})();
