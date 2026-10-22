(() => {
  'use strict';

  const $ = (id) => document.getElementById(id);
  const svgNS = 'http://www.w3.org/2000/svg';
  const numberFormat = new Intl.NumberFormat('ko-KR', { maximumFractionDigits: 1 });
  const integerFormat = new Intl.NumberFormat('ko-KR', { maximumFractionDigits: 0 });
  const axisFormat = new Intl.NumberFormat('ko-KR', { notation: 'compact', maximumFractionDigits: 1 });
  const timeFormat = new Intl.DateTimeFormat('ko-KR', { hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false });
  let adminKey = '';
  let generation = 0;
  let controller = null;
  let inFlight = false;
  let pendingRefresh = false;
  let pollTimer = null;
  let lastSuccess = null;
  let connectionError = false;
  let pageSuspended = false;
  let bootId = null;
  let history = [];
  let latestStatus = null;
  let lastRooms = [];
  let selectedRoom = null;
  let filters = { game: '', visibility: 'all', query: '' };
  let pageCursors = [null];
  let pageIndex = 0;
  let nextCursor = null;

  const text = (id, value) => { $(id).textContent = value; };
  const finite = (value) => typeof value === 'number' && Number.isFinite(value);
  const num = (value, integer = false) => finite(value) ? (integer ? integerFormat : numberFormat).format(value) : '—';
  const age = (value) => finite(value) ? (value < 1000 ? '1초 미만' : `${num(value / 1000)}초 전`) : '측정 대기';
  const stateLabel = (state) => ({ ready: '정상', warming_up: '수집 준비 중', stale: '갱신 지연', stopped: '서버 종료', unavailable: '측정 불가' }[state] || '확인 대기');
  const isVisible = () => !document.hidden && !pageSuspended;
  const autoEnabled = () => $('auto-refresh').checked;
  const currentGeneration = (value) => value === generation && Boolean(adminKey) && isVisible();

  function clearTimer() {
    if (pollTimer !== null) window.clearTimeout(pollTimer);
    pollTimer = null;
  }

  function invalidate() {
    clearTimer();
    generation += 1;
    if (controller) controller.abort();
  }

  function notice(message, error = false) {
    text('notice', message);
    $('notice').classList.toggle('error', error);
    $('notice').hidden = !message;
  }

  function setBusy(busy) {
    $('refresh').disabled = busy;
    $('refresh').textContent = busy ? '갱신 중…' : '지금 갱신';
    $('login-submit').disabled = busy;
    $('login-submit').textContent = busy ? '연결 확인 중…' : '대시보드 열기';
    $('dashboard').setAttribute('aria-busy', String(busy));
    $('first-page').disabled = busy || pageIndex === 0;
    $('previous-page').disabled = busy || pageIndex === 0;
    $('next-page').disabled = busy || !nextCursor;
  }

  function clearData() {
    latestStatus = null;
    history = [];
    bootId = null;
    lastSuccess = null;
    connectionError = false;
    lastRooms = [];
    selectedRoom = null;
    pageCursors = [null];
    pageIndex = 0;
    nextCursor = null;
    filters = { game: '', visibility: 'all', query: '' };
    $('game-filter').replaceChildren(new Option('모든 게임', ''));
    $('visibility-filter').value = 'all';
    $('room-query').value = '';
    $('room-rows').replaceChildren();
    $('room-table-scroll').scrollTop = 0;
    $('runtime-metrics').replaceChildren();
    $('room-details').replaceChildren();
    if ($('room-drawer').open) $('room-drawer').close();
    for (const id of ['metric-connections', 'metric-sessions', 'metric-rooms', 'metric-reserved', 'metric-cpu', 'metric-rss', 'room-count', 'chart-connections-value', 'chart-cpu-value', 'chart-rss-value']) text(id, '—');
    for (const name of ['connections', 'throughput', 'cpu', 'rss']) {
      $(`chart-${name}`).replaceChildren();
      text(`chart-${name}-summary`, '이력 수집 대기 중');
    }
    text('connections-note', '현재 열린 연결');
    text('rooms-note', '공개 / 비공개');
    text('members-note', '연결 인원 —');
    text('cpu-note', '1코어 = 100%');
    text('rss-note', '실제 상주 메모리');
    text('server-description', '서버 데이터를 불러오는 중입니다.');
    text('history-description', '수집된 표본만 표시합니다.');
    text('room-empty', '룸 정보를 불러오는 중입니다.');
    $('room-empty').hidden = false;
    text('page-description', '첫 페이지');
    text('filter-description', '모든 게임 · 전체 공개 설정');
    text('footer-status', '데이터 수집 대기 중');
    for (const id of ['first-page', 'previous-page', 'next-page']) $(id).disabled = true;
    notice('');
  }

  function logout(message = '') {
    invalidate();
    adminKey = '';
    pendingRefresh = false;
    clearData();
    $('admin-key').value = '';
    $('dashboard').hidden = true;
    $('toolbar').hidden = true;
    $('login-panel').hidden = false;
    text('login-error', message);
    setBusy(inFlight);
    updateFreshness();
    $('admin-key').focus();
  }

  function updateFreshness() {
    let label = '로그인 필요';
    let tone = '';
    if (adminKey) {
      if (!isVisible()) { label = '화면 비활성 · 일시중단'; tone = 'warning'; }
      else if (connectionError) { label = '연결 실패'; tone = 'error'; }
      else if (!lastSuccess) { label = '연결 확인 중'; tone = 'warning'; }
      else if (!autoEnabled()) { label = '자동 갱신 꺼짐'; tone = 'warning'; }
      else if (Date.now() - lastSuccess > 10000) { label = '브라우저 갱신 지연'; tone = 'warning'; }
      else if (latestStatus && latestStatus.state !== 'ready') { label = stateLabel(latestStatus.state); tone = 'warning'; }
      else { label = '연결됨'; tone = 'ready'; }
    }
    text('connection-state', label);
    $('connection-dot').className = `dot ${tone}`.trim();
    text('last-update', lastSuccess ? `마지막 조회 ${timeFormat.format(lastSuccess)}` : '조회 전용');
  }

  function schedule() {
    clearTimer();
    if (adminKey && isVisible() && autoEnabled() && !inFlight) pollTimer = window.setTimeout(refresh, 5000);
  }

  class RequestError extends Error {
    constructor(status, code = '') { super('Dashboard request failed'); this.status = status; this.code = code; }
  }

  async function request(path, signal, requestGeneration) {
    const response = await fetch(path, {
      method: 'GET', headers: { Authorization: `Bearer ${adminKey}`, Accept: 'application/json' },
      cache: 'no-store', redirect: 'error', credentials: 'omit', signal,
    });
    if (!currentGeneration(requestGeneration)) throw new DOMException('Request cancelled', 'AbortError');
    if (response.status === 401) {
      logout('관리자 키가 유효하지 않거나 만료되었습니다. 키를 다시 입력하세요.');
      throw new RequestError(401);
    }
    const body = await response.json();
    if (!currentGeneration(requestGeneration)) throw new DOMException('Request cancelled', 'AbortError');
    if (!response.ok) throw new RequestError(response.status, body.error?.code);
    if (body.schema_version !== 1 || typeof body.state !== 'string') throw new RequestError(0, 'schema');
    return body;
  }

  async function refresh() {
    clearTimer();
    if (!adminKey || !isVisible()) return;
    if (inFlight) { pendingRefresh = true; return; }
    inFlight = true;
    pendingRefresh = false;
    const requestGeneration = generation;
    const batchController = new AbortController();
    controller = batchController;
    setBusy(true);
    try {
      const status = await request('/admin/api/status', batchController.signal, requestGeneration);
      if (!currentGeneration(requestGeneration)) return;
      // A successful status response authenticates even before the first sample.
      if ($('dashboard').hidden) {
        $('login-panel').hidden = true;
        $('dashboard').hidden = false;
        $('toolbar').hidden = false;
        text('login-error', '');
        renderStatus(status);
        $('main').focus();
      }
      const newBoot = bootId !== status.boot_id;
      const batchPage = newBoot ? 0 : pageIndex;
      const cursor = newBoot ? null : pageCursors[batchPage];
      const roomQuery = new URLSearchParams({ visibility: filters.visibility, limit: '100' });
      if (filters.game) roomQuery.set('game_id', filters.game);
      if (filters.query) roomQuery.set('q', filters.query);
      if (cursor) roomQuery.set('cursor', cursor);
      const historyQuery = new URLSearchParams({ limit: '900' });
      if (!newBoot && bootId) {
        historyQuery.set('boot_id', bootId);
        if (history.length) historyQuery.set('after_seq', String(history[history.length - 1].sample_seq));
      }
      const [rooms, historyResponse] = await Promise.all([
        request(`/admin/api/rooms?${roomQuery}`, batchController.signal, requestGeneration),
        request(`/admin/api/history?${historyQuery}`, batchController.signal, requestGeneration),
      ]);
      if (!currentGeneration(requestGeneration)) return;
      // A restart between endpoints must never mix histories or room pages.
      if (rooms.boot_id !== status.boot_id || historyResponse.boot_id !== status.boot_id) {
        bootId = null;
        pendingRefresh = true;
        notice('서버가 재시작되었습니다. 새 표본을 확인하는 중입니다.');
        return;
      }
      if (newBoot) {
        history = [];
        $('room-table-scroll').scrollTop = 0;
        pageIndex = 0;
        pageCursors = [null];
        selectedRoom = null;
        if ($('room-drawer').open) $('room-drawer').close();
      }
      bootId = status.boot_id;
      const samples = historyResponse.data?.samples || [];
      const merged = historyResponse.data?.reset ? samples : [...history, ...samples];
      const unique = new Map(merged.map((sample) => [sample.sample_seq, sample]));
      history = [...unique.values()].sort((a, b) => a.sample_seq - b.sample_seq);
      const latestTime = history.length ? history[history.length - 1].collected_at : null;
      history = history.filter((sample) => finite(sample.collected_at) && sample.collected_at >= latestTime - 900000).slice(-900);
      latestStatus = status;
      lastSuccess = Date.now();
      connectionError = false;
      $('login-panel').hidden = true;
      $('dashboard').hidden = false;
      $('toolbar').hidden = false;
      text('login-error', '');
      renderStatus(status);
      renderRooms(rooms);
      renderCharts();
      updateFreshness();
    } catch (error) {
      if (requestGeneration !== generation || !adminKey || error.name === 'AbortError' || error.status === 401) return;
      batchController.abort();
      connectionError = true;
      if (!$('dashboard').hidden) {
        const invalidCursor = error.status === 400 && pageIndex > 0;
        if (invalidCursor) {
          pageIndex = 0;
          pageCursors = [null];
          nextCursor = null;
          notice('룸 페이지가 만료되었습니다. 처음 페이지로 돌아가려면 지금 갱신을 누르세요.', true);
        } else notice('서버 데이터를 가져오지 못했습니다. 마지막으로 받은 값을 표시합니다. 연결을 확인하고 다시 갱신하세요.', true);
      } else text('login-error', '서버에 연결하지 못했습니다. 주소와 네트워크를 확인한 뒤 다시 시도하세요.');
      updateFreshness();
    } finally {
      if (controller === batchController) controller = null;
      inFlight = false;
      setBusy(false);
      if (pendingRefresh && adminKey && isVisible()) {
        pendingRefresh = false;
        refresh();
      } else schedule();
    }
  }

  function renderStatus(envelope) {
    const data = envelope.data;
    if (!data) {
      notice(envelope.state === 'stopped' ? '서버가 종료되었습니다. 보관된 표본이 없습니다.' : '서버에 연결했습니다. 첫 관측 표본을 수집하는 중입니다.');
      text('server-description', stateLabel(envelope.state));
      return;
    }
    const metrics = data.metrics || {};
    const rooms = data.rooms || {};
    const process = data.process || {};
    text('metric-connections', num(metrics.active_connections, true));
    text('metric-sessions', num(metrics.active_sessions, true));
    text('metric-rooms', num(rooms.total, true));
    text('metric-reserved', num(rooms.reserved_members, true));
    text('connections-note', `인증 전 연결 ${num(metrics.unauthenticated_connections, true)}`);
    text('rooms-note', `공개 ${num(rooms.public, true)} / 비공개 ${num(rooms.private, true)}`);
    text('members-note', `연결 인원 ${num(rooms.connected_members, true)}`);
    text('metric-cpu', num(process.cpu_percent));
    text('metric-rss', num(finite(process.rss_bytes) ? process.rss_bytes / 1048576 : null));
    text('cpu-note', process.state === 'ready' ? '1코어 = 100% · 초과 가능' : `${stateLabel(process.state)} · 1코어 = 100%`);
    text('rss-note', process.state === 'ready' ? '실제 상주 메모리' : stateLabel(process.state));
    const uptime = finite(data.uptime_ms) ? `${num(data.uptime_ms / 3600000)}시간` : '—';
    text('server-description', `가동 ${uptime} · 등록 게임 ${num(data.games?.length, true)}개 · 프로세스 ${num(process.pid, true)}`);
    text('footer-status', `서버 표본 ${num(envelope.sample_seq, true)} · cache ${age(envelope.cache_age_ms)} · 자원 ${age(process.age_ms)}`);
    if (data.stopping || envelope.state === 'stopped') notice('서버가 종료 중이거나 종료되었습니다. 마지막 관측 표본을 표시합니다.');
    else if (envelope.state === 'stale') notice('서버의 관측 표본 갱신이 지연되고 있습니다. 마지막 수집 값을 표시합니다.');
    else if (envelope.state === 'warming_up') notice('서버에 연결했습니다. 첫 관측 표본을 수집하는 중입니다.');
    else if (process.state === 'stale' || process.state === 'unavailable') notice(`프로세스 자원 ${stateLabel(process.state)}. 룸과 연결 지표는 계속 갱신합니다.`);
    else notice('');
    const select = $('game-filter');
    const selected = filters.game;
    const games = data.games || [];
    const gameIds = games.map((game) => game.game_id);
    if (selected && !gameIds.includes(selected)) gameIds.push(selected);
    select.replaceChildren(new Option('모든 게임', ''), ...gameIds.map((game) => new Option(game, game)));
    select.value = selected;
    renderRuntime(metrics);
  }

  function pair(label, value) {
    const wrapper = document.createElement('div');
    const term = document.createElement('dt');
    const description = document.createElement('dd');
    term.textContent = label;
    description.textContent = value;
    wrapper.append(term, description);
    return wrapper;
  }

  function renderRuntime(metrics) {
    const fields = [
      ['inputs_processed', '누적 입력 처리', '건'], ['snapshots', '누적 스냅샷 발행', '건'],
      ['admission_rejections', '누적 입장 거절', '건'], ['shared_queue_overloads', '누적 공유 큐 과부하', '건'],
      ['abusive_frames', '누적 비정상 프레임', '건'], ['abusive_inputs', '누적 비정상 입력', '건'],
      ['reliable_queue_disconnects', '누적 reliable 큐 연결 종료', '건'], ['write_deadline_disconnects', '누적 쓰기 기한 연결 종료', '건'],
      ['room_panics', '누적 룸 panic', '건'], ['guests_issued', '누적 게스트 발급', '건'],
      ['max_input_queue_depth', '최대 입력 큐 깊이', '건'], ['max_reliable_queue_depth', '최대 reliable 큐 깊이', '건'],
      ['max_tick_delay_micros', '최대 tick 지연', 'ms'], ['max_tick_duration_micros', '최대 tick 실행 시간', 'ms'],
      ['max_input_latency_micros', '최대 입력 대기', 'ms'], ['max_input_duration_micros', '최대 입력 처리 시간', 'ms'],
    ];
    $('runtime-metrics').replaceChildren(...fields.map(([key, label, unit]) => pair(label, `${num(unit === 'ms' && finite(metrics[key]) ? metrics[key] / 1000 : metrics[key], unit === '건')} ${unit}`)));
  }

  function cell(value, className = '') {
    const element = document.createElement('td');
    element.textContent = value;
    if (className) element.className = className;
    return element;
  }

  function roomKey(room) { return JSON.stringify([room.game_id, room.room_id, room.room_instance_id]); }

  function renderRooms(envelope) {
    const data = envelope.data;
    const scroll = $('room-table-scroll');
    const scrollTop = scroll.scrollTop;
    const focusedRoom = lastRooms.find((room, index) => $('room-rows').children[index]?.children[0]?.children[1] === document.activeElement);
    lastRooms = data?.rooms || [];
    nextCursor = data?.next_cursor || null;
    text('room-count', data ? `${num(data.total_filtered, true)}개` : '—');
    const rows = lastRooms.map((room) => {
      const row = document.createElement('tr');
      const identity = document.createElement('td');
      const game = document.createElement('span');
      game.className = 'game-name'; game.textContent = room.game_id;
      const button = document.createElement('button');
      button.type = 'button'; button.className = 'room-button'; button.textContent = room.room_id;
      button.setAttribute('aria-label', `${room.game_id} 게임의 ${room.room_id} 룸 상세 보기`);
      button.addEventListener('click', () => openRoom(room));
      identity.append(game, button);
      const state = cell('');
      const badge = document.createElement('span');
      badge.className = `status-badge ${room.state === 'ready' ? '' : 'warning'}`;
      badge.textContent = stateLabel(room.state);
      state.append(badge);
      row.append(identity, cell(room.public ? '공개' : '비공개', room.public ? '' : 'private-tag'), cell(`${num(room.connected_members, true)} / ${num(room.capacity, true)}`, 'numeric'), cell(num(room.reserved_members, true), 'numeric'), cell(num(room.server_tick, true), 'numeric'), cell(age(room.summary_age_ms)), state);
      return row;
    });
    $('room-rows').replaceChildren(...rows);
    if (focusedRoom) {
      const index = lastRooms.findIndex((room) => roomKey(room) === roomKey(focusedRoom));
      if (index >= 0) rows[index].children[0].children[1].focus({ preventScroll: true });
      else scroll.focus({ preventScroll: true });
    }
    scroll.scrollTop = scrollTop;
    $('room-empty').hidden = rows.length > 0;
    if (!rows.length) {
      let message = '현재 활성 룸이 없습니다. 게임에서 룸을 생성하면 여기에 표시됩니다.';
      if (envelope.state === 'warming_up' || !data) message = '첫 룸 관측 표본을 수집하는 중입니다.';
      else if (pageIndex > 0) message = '이 페이지의 룸이 종료되었거나 변경되었습니다. 처음 페이지에서 다시 확인하세요.';
      else if (filters.game || filters.visibility !== 'all' || filters.query) message = '필터에 맞는 룸이 없습니다. 필터를 변경하거나 초기화하세요.';
      text('room-empty', message);
    }
    text('page-description', `${pageIndex + 1}페이지 · ${num(rows.length, true)}개 표시`);
    const filterLabels = [filters.game || '모든 게임', { all: '전체 공개 설정', public: '공개 룸', private: '비공개 룸' }[filters.visibility]];
    if (filters.query) filterLabels.push(`룸 ID: ${filters.query}`);
    text('filter-description', filterLabels.join(' · '));
    $('first-page').disabled = pageIndex === 0;
    $('previous-page').disabled = pageIndex === 0;
    $('next-page').disabled = !nextCursor;
    if (selectedRoom && $('room-drawer').open) {
      const current = lastRooms.find((room) => roomKey(room) === selectedRoom);
      if (current) renderRoomDetails(current);
      else {
        $('room-details').replaceChildren(pair('룸 상태', '현재 페이지에서 더 이상 관측되지 않습니다. 룸이 종료되거나 페이지가 변경되었을 수 있습니다.'));
      }
    }
  }

  function renderRoomDetails(room) {
    text('drawer-title', '룸 상세');
    $('room-details').replaceChildren(
      pair('게임', room.game_id), pair('룸 ID', room.room_id), pair('룸 instance ID', room.room_instance_id),
      pair('공개 설정', room.public ? '공개' : '비공개'), pair('정원', num(room.capacity, true)),
      pair('연결 인원', num(room.connected_members, true)), pair('재접속 예약석', num(room.reserved_members, true)),
      pair('서버 tick', num(room.server_tick, true)), pair('스냅샷 sequence', num(room.snapshot_seq, true)),
      pair('최근 갱신', age(room.summary_age_ms)), pair('관측 상태', stateLabel(room.state)),
    );
  }

  function openRoom(room) {
    selectedRoom = roomKey(room);
    renderRoomDetails(room);
    $('room-drawer').showModal();
  }

  function svgElement(name, attributes, value) {
    const element = document.createElementNS(svgNS, name);
    for (const [key, content] of Object.entries(attributes)) element.setAttribute(key, String(content));
    if (value !== undefined) element.textContent = value;
    return element;
  }

  function historyRange() {
    const end = history[history.length - 1].collected_at;
    const observed = Math.max(0, end - history[0].collected_at);
    const span = Math.min(900000, Math.max(10000, observed));
    const seconds = Math.floor(observed / 1000);
    const duration = seconds < 60 ? `${num(seconds, true)}초` : `${Math.floor(seconds / 60)}분 ${seconds % 60}초`;
    return { start: end - span, end, span, description: observed ? `관측 ${duration}` : '관측 시작' };
  }

  function chart(name, series, unit) {
    const target = $(`chart-${name}`);
    target.replaceChildren();
    target.setAttribute('aria-describedby', `chart-${name}-summary`);
    const valid = series.flatMap((line) => history.map((sample) => line.get(sample))).filter(finite);
    const summaryId = `chart-${name}-summary`;
    if (!history.length || !valid.length) {
      const unavailable = history.length && ['cpu', 'rss'].includes(name);
      text(summaryId, unavailable ? '자원 측정 표본 없음 · 수집 상태를 확인하세요.' : '이력 수집 대기 중');
      target.setAttribute('aria-label', `${series.map((line) => line.label).join(', ')} · ${unavailable ? '측정값 없음' : '수집 대기'}`);
      target.append(svgElement('text', { x: 240, y: 75, 'text-anchor': 'middle', class: 'chart-label' }, unavailable ? '측정값 없음' : '수집 대기'));
      return;
    }
    const { start, end, span, description } = historyRange();
    target.setAttribute('aria-label', `${series.map((line) => line.label).join(', ')} · ${history.length}개 표본 · ${description} · ${timeFormat.format(start)}부터 ${timeFormat.format(end)}까지`);
    const max = Math.max(1, ...valid) * 1.1;
    const x = (timestamp) => 58 + (timestamp - start) / span * 408;
    const y = (value) => 105 - value / max * 90;
    for (const value of [0, max / 2, max]) {
      target.append(svgElement('line', { x1: 58, y1: y(value), x2: 466, y2: y(value), class: 'chart-gridline' }));
      target.append(svgElement('text', { x: 49, y: y(value) + 4, 'text-anchor': 'end', class: 'chart-label' }, axisFormat.format(value)));
    }
    for (const [timestamp, anchor] of [[start, 'start'], [start + span / 2, 'middle'], [end, 'end']]) {
      target.append(svgElement('text', { x: x(timestamp), y: 133, 'text-anchor': anchor, class: 'chart-label chart-time' }, timeFormat.format(timestamp)));
    }
    series.forEach((line, index) => {
      let path = '';
      let previous = null;
      for (const sample of history) {
        const value = line.get(sample);
        if (!finite(value)) { previous = null; continue; }
        const gap = previous === null || sample.collected_at - previous > 2500;
        path += `${gap ? 'M' : 'L'}${x(sample.collected_at).toFixed(2)},${y(value).toFixed(2)} `;
        if (gap) target.append(svgElement('circle', { cx: x(sample.collected_at), cy: y(value), r: 2.5, class: `chart-point${index ? ' secondary' : ''}` }));
        previous = sample.collected_at;
      }
      target.append(svgElement('path', { d: path, class: `chart-line${index ? ' secondary' : ''}` }));
    });
    const summaries = series.map((line) => {
      const values = history.map(line.get).filter(finite);
      if (!values.length) return `${line.label} 측정 대기`;
      const last = line.get(history[history.length - 1]);
      return `${line.label} 최신 ${num(last)}${unit} / 최대 ${num(Math.max(...values))}${unit}`;
    });
    text(summaryId, summaries.join(' · '));
  }

  function renderCharts() {
    chart('connections', [{ label: '연결', get: (sample) => sample.metrics?.active_connections }], '개');
    chart('throughput', [
      { label: '입력', get: (sample) => sample.rates?.inputs_per_second },
      { label: '발행', get: (sample) => sample.rates?.snapshots_per_second },
    ], '건/초');
    chart('cpu', [{ label: 'CPU', get: (sample) => sample.process?.cpu_percent }], '%');
    chart('rss', [{ label: 'RSS', get: (sample) => finite(sample.process?.rss_bytes) ? sample.process.rss_bytes / 1048576 : null }], ' MiB');
    const last = history[history.length - 1];
    text('chart-connections-value', last ? `${num(last.metrics?.active_connections, true)}개` : '—');
    text('chart-cpu-value', last ? `${num(last.process?.cpu_percent)}%` : '—');
    text('chart-rss-value', last ? `${num(finite(last.process?.rss_bytes) ? last.process.rss_bytes / 1048576 : null)} MiB` : '—');
    text('history-description', history.length ? `${num(history.length, true)}개 표본 · ${historyRange().description} · 빈 구간은 연결하지 않습니다.` : '첫 이력 표본을 수집하는 중입니다.');
  }

  function changePage(index, cursor) {
    if (cursor !== undefined) pageCursors[index] = cursor;
    pageIndex = index;
    $('room-table-scroll').scrollTop = 0;
    invalidate();
    refresh();
  }

  $('login-form').addEventListener('submit', (event) => {
    event.preventDefault();
    if (inFlight) return;
    const key = $('admin-key').value;
    $('admin-key').value = '';
    if (!key) return;
    invalidate();
    adminKey = key;
    clearData();
    text('login-error', '');
    updateFreshness();
    refresh();
  });
  $('logout').addEventListener('click', () => logout());
  $('refresh').addEventListener('click', refresh);
  $('auto-refresh').addEventListener('change', () => {
    clearTimer();
    updateFreshness();
    if (autoEnabled()) refresh();
  });
  $('filters').addEventListener('submit', (event) => {
    event.preventDefault();
    filters = { game: $('game-filter').value, visibility: $('visibility-filter').value, query: $('room-query').value.trim() };
    pageCursors = [null];
    changePage(0);
  });
  $('reset-filters').addEventListener('click', () => {
    filters = { game: '', visibility: 'all', query: '' };
    $('game-filter').value = '';
    $('visibility-filter').value = 'all';
    $('room-query').value = '';
    pageCursors = [null];
    changePage(0);
  });
  $('next-page').addEventListener('click', () => { if (nextCursor) changePage(pageIndex + 1, nextCursor); });
  $('previous-page').addEventListener('click', () => { if (pageIndex > 0) changePage(pageIndex - 1); });
  $('first-page').addEventListener('click', () => changePage(0));
  $('close-drawer').addEventListener('click', () => $('room-drawer').close());
  $('room-drawer').addEventListener('close', () => { selectedRoom = null; });
  document.addEventListener('visibilitychange', () => {
    if (!isVisible()) { invalidate(); pendingRefresh = false; }
    else if (adminKey && autoEnabled()) refresh();
    updateFreshness();
  });
  window.addEventListener('pagehide', () => {
    pageSuspended = true;
    invalidate();
    pendingRefresh = false;
    updateFreshness();
  });
  window.addEventListener('pageshow', () => {
    if (!pageSuspended) return;
    pageSuspended = false;
    if (adminKey && autoEnabled()) refresh();
    updateFreshness();
  });
  window.setInterval(updateFreshness, 1000);
  updateFreshness();
})();
