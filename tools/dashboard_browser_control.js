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
    return response;
  };
  window.__dashboardQA = qa;
  return 'Controlled race/visibility fixture installed; headers are never retained';
})();
