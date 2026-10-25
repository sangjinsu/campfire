(() => {
  // Browser-only history fixture. Keep the real response envelope and alter history samples.
  const original = window.fetch.bind(window);
  const make = (sequence, time, cpu = 20) => ({
    sample_seq: sequence,
    collected_at: time,
    metrics: { active_connections: sequence },
    rates: { inputs_per_second: sequence * 2, snapshots_per_second: sequence },
    process: { cpu_percent: cpu, rss_bytes: 10485760 },
  });
  window.__historyCase = 'eleven';
  window.fetch = async (url, options) => {
    const response = await original(url, options);
    if (!String(url).includes('/history')) return response;
    const body = await response.json();
    const now = Date.now();
    let samples = [];
    switch (window.__historyCase) {
      case 'one': samples = [make(1, now)]; break;
      case 'eleven': samples = Array.from({ length: 11 }, (_, index) => make(index + 1, now - 10000 + index * 1000)); break;
      case 'fifteen': samples = [make(1, now - 900000), make(901, now)]; break;
      case 'gap-null-multicore': samples = [make(1, now - 5000, 0), make(2, now - 4000, null), make(3, now, 125)]; break;
    }
    body.data = { ...body.data, samples, reset: true, oldest_seq: samples.length ? samples[0].sample_seq : null };
    return new Response(JSON.stringify(body), { status: response.status, headers: { 'Content-Type': 'application/json' } });
  };
  return 'Controlled history fixture installed';
})();
