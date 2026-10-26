// AFTERLIFE 렌더러 — 오디오 터미널 UI.
// 실제 앱: 검색·주소 추출은 yt-dlp, 재생은 libmpv (둘 다 Rust 쪽 포터블).
// 브라우저 미리보기: 가짜 데이터(mock.js)와 타이머로 흉내만 낸다.

const REAL = !!window.__TAURI__;
const invoke = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);

const Service = REAL ? { search: query => invoke('yt_search', { query }) } : MockService;

// libmpv 플레이어 (Rust: player.rs). 미리보기에서는 null → 가짜 시계 사용
const trackUrl = t => t.url || `https://www.youtube.com/watch?v=${t.id}`;
const Player = REAL ? {
  // mode: 'normal'(미리 뽑아 둔 주소 사용) · 'fresh'(새로 뽑기) · 'download'(yt-dlp 로 파일을 받아 재생)
  load: (t, mode = 'normal') => invoke('player_load', { url: trackUrl(t), mode }),
  // 오디오 주소를 미리 뽑아 둠 → 그 곡을 누르면 바로 시작 (Rust 캐시)
  prefetch: t => invoke('yt_prefetch', { url: trackUrl(t) }).catch(() => {}),
  pause: paused => invoke('player_pause', { paused }),
  seek: pos => invoke('player_seek', { pos }),
  volume: volume => invoke('player_set_volume', { volume }),
  loop: on => invoke('player_set_loop', { on }),
} : null;

// 재생 판단을 player.log 에 남김 (실제 앱만)
const plog = msg => REAL && invoke('player_log', { msg }).catch(() => {});

// Rust 오류 문자열을 토스트용으로 짧게
const errText = e => String(e?.message || e).slice(0, 48);

const $ = sel => document.querySelector(sel);

/* ───────── 볼륨 ─────────
   화면 표시값: 0.0 ~ 100.0 (소수점 첫째 자리), 기본 50.0
   실제 출력값: 표시값 × 0.75  →  표시 100 = 실제 75%
   engineVolume() 결과를 mpv "volume" 으로 보낸다 (updateVolume) */
const VOLUME_DEFAULT = 50;
const VOLUME_CEILING = 0.75;

const clampVolume = v => Math.min(100, Math.max(0, Math.round(v * 10) / 10));
const engineVolume = () => (state.muted ? 0 : state.volume * VOLUME_CEILING);

// 목록에 보여줄 최대 곡 수 (Rust 검색도 ytsearch10 으로 맞춤)
const MAX_TRACKS = 10;
const capList = tracks => tracks.slice(0, MAX_TRACKS);

/* ───────── 라이브러리 (좋아요 · 플레이리스트) ─────────
   지금은 웹뷰 localStorage에 저장. TODO: Rust 쪽 파일 저장으로 옮기기 */
const LIBRARY_KEY = 'afterlife.library.v1';
const library = loadLibrary();

function loadLibrary() {
  try {
    const d = JSON.parse(localStorage.getItem(LIBRARY_KEY));
    if (d && Array.isArray(d.liked) && Array.isArray(d.playlists)) {
      if (!Array.isArray(d.history)) d.history = [];
      return d;
    }
  } catch {}
  return { liked: [], playlists: [], history: [] };
}

function saveLibrary() {
  try { localStorage.setItem(LIBRARY_KEY, JSON.stringify(library)); } catch {}
  renderDrawer(); // 메뉴의 개수 표시 갱신
}

// 검색 결과는 사라지므로 곡 정보를 통째로 저장
const trackData = t => ({ id: t.id, title: t.title, artist: t.artist, duration: t.duration, thumbnail: t.thumbnail, url: t.url, source: t.source });
const hasTrack = (list, t) => list.some(x => x.id === t.id);
const isLiked = t => hasTrack(library.liked, t);
const findPlaylist = id => library.playlists.find(p => p.id === id);

function toggleLike(t) {
  if (!t) return toast('NO SIGNAL · 재생 중인 곡 없음');
  const liked = isLiked(t);
  library.liked = liked ? library.liked.filter(x => x.id !== t.id) : [trackData(t), ...library.liked];
  saveLibrary();
  toast(liked ? 'UNLIKED' : 'LIKED');
  updateDeck();
  if (state.source === 'liked') renderList();
}

function togglePlaylistTrack(pl, t) {
  const has = hasTrack(pl.tracks, t);
  pl.tracks = has ? pl.tracks.filter(x => x.id !== t.id) : [...pl.tracks, trackData(t)];
  saveLibrary();
  toast(`${has ? 'REMOVED FROM' : 'ADDED TO'} · ${pl.name.toUpperCase()}`);
  if (state.source === pl.id) renderList();
}

function createPlaylist(name, firstTrack) {
  const pl = { id: 'pl' + Date.now(), name: name.trim().slice(0, 40), tracks: firstTrack ? [trackData(firstTrack)] : [] };
  library.playlists.push(pl);
  saveLibrary();
  return pl;
}

const HISTORY_LIMIT = 50;

function addHistory(t) {
  library.history = [trackData(t), ...library.history.filter(x => x.id !== t.id)].slice(0, HISTORY_LIMIT);
  saveLibrary();
  if (state.source === 'history') renderList();
}

function deletePlaylist(pl) {
  library.playlists = library.playlists.filter(p => p !== pl);
  saveLibrary();
  if (state.source === pl.id) setSource('signals');
  toast(`DELETED · ${pl.name.toUpperCase()}`);
}

/* ───────── State ───────── */
const state = {
  list: [],                   // 검색 결과만 (최대 10곡)
  source: 'signals',          // 보고 있는 목록: 'signals' | 'history' | 'liked' | 'settings' | 플레이리스트 id
  query: '',
  scanning: false,

  queue: [],                  // 재생을 시작한 목록
  index: -1,
  playing: false,
  loading: false,
  position: 0,
  repeat: 'off',              // 반복: 'off' | 'all'(목록 전체) | 'one'(한 곡)
  queueSource: 'signals',     // 재생을 시작한 목록 (검색 결과면 한 곡만, 그 외는 이어 재생)

  volume: VOLUME_DEFAULT,
  muted: false,
};

const engines = { checked: false, ready: false, ytdlp: null, mpv: null };

const current = () => state.queue[state.index] || null;

/* ───────── Helpers ───────── */
const pad2 = n => String(n).padStart(2, '0');

function fmt(sec) {
  sec = Math.max(0, Math.floor(sec || 0));
  return `${pad2(Math.floor(sec / 60))}:${pad2(sec % 60)}`;
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

let toastTimer;
function toast(msg) {
  const t = $('#toast');
  t.textContent = msg;
  t.classList.add('show');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.remove('show'), 1800);
}

function setEngineDot(status) {
  $('#engine-dot').className = `engine-dot ${status}`;
}

function restoreEngineDot() {
  setEngineDot(engines.ready ? 'ready' : engines.checked ? 'missing' : '');
}

/* ───────── 트랙 목록 ───────── */
const ICON_PLAY = '<svg viewBox="0 0 24 24" class="play-i"><circle cx="12" cy="12" r="9"/><path d="M10.5 9v6l4.5-3z"/></svg>';
const ICON_WAVE = '<span class="wave"><i></i><i></i><i></i><i></i><i></i></span>';
const ICON_NOTE = '<svg viewBox="0 0 24 24"><path d="M9 18V5l11-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="17" cy="16" r="3"/></svg>';
const ICON_REMOVE = '<svg viewBox="0 0 24 24"><path d="M6 12h12"/></svg>';
const ICON_ADD = '<svg viewBox="0 0 24 24"><path d="M4 6h11M4 11h11M4 16h7M17.5 13.5v7M14 17h7"/></svg>';

// 지금 보고 있는 목록
function visibleList() {
  if (state.source === 'signals') return state.list;
  if (state.source === 'liked') return library.liked;
  if (state.source === 'history') return library.history;
  return findPlaylist(state.source)?.tracks || [];
}

function sourceTitle() {
  if (state.source === 'settings') return 'SETTINGS';
  if (state.source === 'history') return 'HISTORY';
  if (state.source === 'liked') return 'LIKED SIGNALS';
  const pl = findPlaylist(state.source);
  if (pl) return pl.name.toUpperCase();
  return state.query ? `SCAN RESULT · ${state.query.toUpperCase()}` : 'AVAILABLE SIGNALS';
}

// 지금 보고 있는 좋아요 / 플레이리스트에서 곡 빼기
function removeFromSource(t) {
  if (state.source === 'liked') return toggleLike(t);
  if (state.source === 'history') {
    library.history = library.history.filter(x => x.id !== t.id);
    saveLibrary();
    return renderList();
  }
  const pl = findPlaylist(state.source);
  if (pl) togglePlaylistTrack(pl, t);
}

function setSource(src) {
  state.source = src;
  closePopover();
  renderList();
  renderDrawer();
  $('#track-list').scrollTop = 0;
}

// 정사각형 섬네일 (yt-dlp thumbnail URL, 없으면 자리표시)
function thumbHtml(t) {
  if (!t.thumbnail) return `<span class="thumb empty">${ICON_NOTE}</span>`;
  return `<span class="thumb"><img src="${escapeHtml(t.thumbnail)}" alt="" loading="lazy" referrerpolicy="no-referrer"></span>`;
}

function renderList() {
  const ul = $('#track-list');
  ul.innerHTML = '';

  $('#signals-title').textContent = sourceTitle();
  if (state.source === 'settings') return renderSettings(ul);

  const list = visibleList();
  const scanning = state.scanning && state.source === 'signals';
  $('#btn-play-all').classList.toggle('show', state.source !== 'signals' && list.length > 0);
  $('#signals-count').textContent = scanning ? '-- TRACKS' : `${pad2(list.length)} TRACKS`;

  if (scanning) {
    ul.innerHTML = '<li class="list-msg scanning">Scanning frequencies</li>';
    return;
  }
  if (!list.length) {
    const msg = state.source === 'signals'
      ? (state.query ? 'No signal found — ESC 로 돌아가기' : 'Scan for signals — 곡 이름이나 YouTube 링크 입력')
      : state.source === 'liked' ? 'No liked signals — 하단 ♥ 로 추가'
      : state.source === 'history' ? 'No history — 재생한 곡이 여기에 쌓임'
      : 'Empty playlist — 곡 옆 + 버튼으로 추가';
    ul.innerHTML = `<li class="list-msg">${msg}</li>`;
    return;
  }

  const cur = current();
  const removable = state.source !== 'signals';
  list.forEach((t, i) => {
    const li = document.createElement('li');
    li.className = 'track';
    li.dataset.id = t.id;
    const active = cur && cur.id === t.id;
    li.classList.toggle('active', !!active);
    li.classList.toggle('playing', !!active && state.playing);
    li.innerHTML = `
      ${thumbHtml(t)}
      <div class="meta">
        <div class="line1">
          <span class="num">${pad2(i + 1)}</span>
          <span class="title">${escapeHtml(t.title)}</span>
        </div>
        <div class="sub">${escapeHtml(t.artist)} / ${fmt(t.duration)}</div>
      </div>
      ${removable ? `<button class="remove" title="${({ liked: '좋아요 취소', history: '기록에서 빼기' })[state.source] || '이 플레이리스트에서 빼기'}">${ICON_REMOVE}</button>` : ''}
      <button class="add" title="플레이리스트에 추가">${ICON_ADD}</button>
      <span class="icon">${ICON_PLAY}${ICON_WAVE}</span>`;
    const img = li.querySelector('.thumb img');
    if (img) img.onerror = () => { img.parentNode.classList.add('empty'); img.replaceWith(document.createRange().createContextualFragment(ICON_NOTE)); };
    li.querySelector('.remove')?.addEventListener('click', e => { e.stopPropagation(); removeFromSource(t); });
    const addBtn = li.querySelector('.add');
    addBtn.onclick = e => { e.stopPropagation(); openPlaylistMenu(addBtn, t); };
    // 곡 위에 잠깐 머물면 주소를 미리 뽑아 둠 (누를 때 바로 시작)
    let hoverTimer;
    li.onmouseenter = () => {
      if (!Player || current()?.id === t.id) return;
      hoverTimer = setTimeout(() => Player.prefetch(t), 600); // 스쳐 지나갈 때는 요청 안 함
    };
    li.onmouseleave = () => clearTimeout(hoverTimer);
    li.onclick = () => {
      clearTimeout(hoverTimer);
      if (current() && current().id === t.id) {
        // 이미 재생한 곡이어도 지금 보는 목록을 이어 재생 목록으로 (다른 목록에서 시작한 곡일 수 있음)
        state.queue = list.slice();
        state.index = i;
        state.queueSource = state.source;
        togglePlay();
      } else playFrom(list, i);
    };
    ul.append(li);
    setMarquee(li.querySelector('.title'), t.title);
  });
}

/* 긴 제목: 넘칠 때만 옆으로 계속 흐르게 (같은 글 두 번 이어 붙여 끊김 없이 반복) */
const MARQUEE_SPEED = 32; // px / 초 — 제목 길이와 상관없이 같은 속도

function setMarquee(el, text) {
  el.dataset.mq = text;
  el.classList.remove('marquee');
  el.textContent = text;
  if (el.scrollWidth - el.clientWidth <= 1) return; // 다 들어가면 그대로
  const safe = escapeHtml(text);
  el.classList.add('marquee');
  el.innerHTML = `<span class="mq-inner"><span>${safe}</span><span aria-hidden="true">${safe}</span></span>`;
  const loop = el.querySelector('.mq-inner > span').offsetWidth; // 글 + 간격 한 바퀴
  el.style.setProperty('--mq-dur', `${(loop / MARQUEE_SPEED).toFixed(2)}s`);
}

// 창 크기가 바뀌면 넘치는지 다시 계산
let marqueeTimer;
window.addEventListener('resize', () => {
  clearTimeout(marqueeTimer);
  marqueeTimer = setTimeout(() => {
    document.querySelectorAll('[data-mq]').forEach(el => setMarquee(el, el.dataset.mq));
  }, 150);
});

// SETTINGS: 엔진만 (yt-dlp · libmpv 포터블 버전 확인 · 설치 · 업데이트)
//   yt-dlp: 없거나 오래되면 시작할 때 자동 업데이트
//   libmpv: 없으면 자동 설치, 새 버전은 [ CHECK ] 로만 (용량이 커서) · 재생 중이면 다음 실행 때 적용
const tools = {
  ytdlp: { label: 'yt-dlp', check: 'ytdlp_check', update: 'ytdlp_update', autoUpdate: true,
    phase: 'idle', version: null, latest: null, progress: 0, restart: false },
  mpv: { label: 'libmpv', check: 'mpv_check', update: 'mpv_update', autoUpdate: false,
    phase: 'idle', version: null, latest: null, progress: 0, restart: false },
};

function toolText(tl) {
  if (!REAL) return ['PREVIEW', false];
  const v = tl.version;
  switch (tl.phase) {
    case 'checking': return ['CHECKING...', false];
    case 'updating': return [`${v ? 'UPDATING' : 'INSTALLING'} ${Math.floor(tl.progress * 100)}%`, false];
    case 'error': return [v ? `${v} · CHECK FAILED` : 'NOT INSTALLED · OFFLINE', false];
  }
  if (!engines.checked) return ['CHECKING...', false];
  if (!v) return ['NOT INSTALLED', false];
  if (tl.restart) return [`${v} · RESTART TO APPLY`, true];
  if (tl.latest && tl.latest !== v) return [`${v} · NEW ${tl.latest}`, true];
  return [tl.latest ? `${v} · LATEST` : v, true];
}

function renderSettings(ul) {
  $('#signals-count').textContent = '';
  ul.innerHTML = '<li class="set-group">ENGINE</li>';
  for (const [key, tl] of Object.entries(tools)) {
    const [text, ok] = toolText(tl);
    const busy = tl.phase === 'checking' || tl.phase === 'updating';
    const li = document.createElement('li');
    li.className = 'set-row';
    li.innerHTML = `
      <span class="k">${tl.label}</span>
      <span class="v${ok ? ' ok' : ''}" id="${key}-value">${escapeHtml(text)}</span>
      ${REAL && !busy ? '<button class="set-btn">[ CHECK ]</button>' : ''}`;
    li.querySelector('.set-btn')?.addEventListener('click', () => ensureTool(key, true));
    ul.append(li);
  }
}

const refreshSettings = () => state.source === 'settings' && renderList();

// GitHub 최신 버전과 비교 → 필요하면 받아서 설치 (Rust: ytdlp.rs · mpvlib.rs)
async function ensureTool(key, manual = false) {
  const tl = tools[key];
  if (!REAL || tl.phase === 'checking' || tl.phase === 'updating') return;
  tl.phase = 'checking';
  refreshSettings();
  try {
    const info = await invoke(tl.check);
    tl.version = info.version;
    tl.latest = info.latest;
    const need = !info.version || (info.update_available && (tl.autoUpdate || manual));
    if (need) {
      const had = info.version;
      tl.phase = 'updating';
      tl.progress = 0;
      refreshSettings();
      const res = await invoke(tl.update);
      tl.version = typeof res === 'string' ? res : res.version;
      tl.restart = !!res.restart_required;
      toast(`${tl.label.toUpperCase()} ${had ? 'UPDATED' : 'INSTALLED'} · ${tl.version}`);
    } else if (manual) {
      toast(`${tl.label.toUpperCase()} · LATEST (${info.latest})`);
    }
    tl.phase = 'idle';
  } catch (err) {
    tl.phase = 'error';
    console.warn(tl.label, err);
    if (manual) toast(`${tl.label.toUpperCase()} · CHECK FAILED`);
  }
  syncEngines();
}

function syncEngines() {
  engines.ytdlp = tools.ytdlp.version;
  engines.mpv = tools.mpv.version;
  engines.ready = !!(engines.ytdlp && engines.mpv);
  updateEngineTitle();
  if (!state.loading && !state.scanning) restoreEngineDot();
  refreshSettings();
}

function updateEngineTitle() {
  $('#engine-dot').title = `yt-dlp: ${engines.ytdlp || '없음'}\nlibmpv: ${engines.mpv || '없음'}`;
}

// 활성 곡 표시만 갱신
function refreshActive() {
  const cur = current();
  document.querySelectorAll('.track').forEach(li => {
    const active = cur && li.dataset.id === cur.id;
    li.classList.toggle('active', !!active);
    li.classList.toggle('playing', !!active && state.playing);
  });
}

/* ───────── 검색 ───────── */
async function scan(query) {
  query = query.trim();
  if (!query) return clearScan();
  state.query = query;
  state.scanning = true;
  state.source = 'signals';
  closePopover();
  setEngineDot('loading');
  renderList();

  let results = [];
  try {
    results = await Service.search(query);
  } catch (err) {
    if (state.query === query) toast(`SCAN FAILED · ${errText(err)}`);
  }
  if (state.query !== query) return; // 더 최신 검색이 있음
  state.scanning = false;
  state.list = capList(results);
  restoreEngineDot();
  renderList();
}

function clearScan() {
  state.query = '';
  state.scanning = false;
  state.list = [];
  $('#scan-input').value = '';
  renderList();
}

/* ───────── 재생 ───────── */
function playFrom(list, i) {
  state.queue = list.slice(); // 재생 중에 목록이 바뀌어도 순서는 그대로
  state.queueSource = state.source;
  state.index = i;
  skippedInRow = 0;
  load();
}

let loadToken = 0;
async function load() {
  const t = current();
  if (!t) return;
  const token = ++loadToken;
  state.position = 0;
  state.playing = false;
  state.loading = true;
  setEngineDot('loading');
  updateDeck();
  refreshActive();

  if (REAL) {
    // Rust: yt-dlp 로 오디오 주소를 뽑아 libmpv 로 재생 → 준비되면 player://loaded
    // 처음: 미리 뽑아 둔 주소 · 1번째 재시도: 새로 뽑기 · 2번째 재시도: 파일로 받아서 재생
    const attempt = retry.id === t.id ? retry.n : 0;
    try {
      await Player.load(t, ['normal', 'fresh', 'download'][attempt] || 'download');
    } catch (err) {
      if (token !== loadToken) return;
      onLoadFailed(t, String(err)); // yt-dlp 주소 추출 실패 등
    }
    return;
  }

  const stream = await Service.resolveStream(t);
  if (token !== loadToken) return; // 그 사이 다른 곡이 선택됨
  if (stream.duration) t.duration = stream.duration;
  onLoaded();
}

// 재생 시작됨 (실제 앱은 player://loaded 이벤트에서)
function onLoaded() {
  if (!state.loading) return;
  state.loading = false;
  state.playing = true;
  retry = { id: null, n: 0 };
  skippedInRow = 0;
  if (current()) addHistory(current());
  prefetchNext();
  restoreEngineDot();
  updateDeck();
  refreshActive();
}

function togglePlay() {
  const t = current();
  if (!t) return visibleList().length && playFrom(visibleList(), 0);
  if (state.loading) return;
  if (!state.playing && t.duration && state.position >= t.duration - 0.5) {
    state.position = 0; // 끝난 곡은 처음부터
    Player?.seek(0);
  }
  state.playing = !state.playing;
  Player?.pause(!state.playing);
  updateDeck();
  refreshActive();
}

// 반복 버튼: 끄기 → 전체 → 한 곡 → 끄기
const REPEAT_NEXT = { off: 'all', all: 'one', one: 'off' };
const REPEAT_LABEL = { off: 'REPEAT · OFF', all: 'REPEAT · ALL', one: 'REPEAT · ONE' };

function toggleRepeat() {
  state.repeat = REPEAT_NEXT[state.repeat];
  Player?.loop(state.repeat === 'one'); // 한 곡 반복은 mpv 가 직접
  updateDeck();
  toast(REPEAT_LABEL[state.repeat]);
}

// 곡이 끝났을 때
//   한 곡 반복: 처음부터 (실제 앱은 mpv loop-file 이 처리해서 여기까지 오지 않음)
//   PLAYLIST · LIKED · HISTORY 에서 시작: 다음 곡 자동 재생, 마지막 곡 뒤에는 전체 반복이면 첫 곡, 아니면 정지
//   검색 결과에서 시작: 그 곡만 (전체 반복이면 그 곡을 다시)
function onTrackEnd() {
  const t = current();
  plog(`곡 끝: #${state.index + 1}/${state.queue.length} ${t?.title} · 목록=${state.queueSource} · 반복=${state.repeat}`);
  if (state.repeat === 'one') { state.position = 0; return; }
  if (advanceQueue()) return;

  if (state.repeat === 'all' && !isContinuous()) {
    // 한 곡만 재생 중인 전체 반복: 그 곡을 다시
    state.position = 0;
    if (REAL) { Player.seek(0); Player.pause(false); } // keep-open 으로 끝에서 멈춰 있던 곡을 다시
    return updateProgress();
  }
  state.playing = false;
  state.position = t.duration;
  updateDeck();
  refreshActive();
}

// 목록 이어 재생 중인가 (검색 결과에서 시작했거나 한 곡뿐이면 아님)
const isContinuous = () => state.queueSource !== 'signals' && state.queue.length > 1;

// 목록 이어 재생 중이면 다음 곡 주소를 미리 뽑아 둠 → 곡이 끝나면 바로 이어짐
function prefetchNext() {
  if (!Player || !isContinuous() || state.repeat === 'one') return;
  const last = state.index >= state.queue.length - 1;
  if (last && state.repeat !== 'all') return;
  Player.prefetch(state.queue[last ? 0 : state.index + 1]);
}

// 다음 곡으로 (마지막 곡 뒤에는 전체 반복일 때만 첫 곡으로). 넘어갔으면 true
function advanceQueue() {
  if (!isContinuous()) return false;
  const last = state.index >= state.queue.length - 1;
  if (last && state.repeat !== 'all') return false;
  state.index = last ? 0 : state.index + 1;
  load();
  return true;
}

/* 곡을 열지 못했을 때 (yt-dlp 주소 추출 실패 · mpv 열기 실패)
   YouTube 가 가끔 일시적으로 거부하므로 주소를 새로 뽑아 최대 2번 다시 시도 →
   그래도 안 되면 목록 이어 재생 중이면 건너뛰고 다음 곡, 아니면 멈춤 */
const MAX_RETRY = 2;
let retry = { id: null, n: 0 };
let skippedInRow = 0; // 연속으로 건너뛴 곡 수 (목록 전체가 실패할 때 끝없이 돌지 않게)

function onLoadFailed(t, message) {
  if (retry.id !== t.id) retry = { id: t.id, n: 0 };
  if (retry.n < MAX_RETRY) {
    retry.n++;
    plog(`다시 시도 ${retry.n}/${MAX_RETRY}: ${t.title} · ${message}`);
    return load();
  }
  plog(`포기: ${t.title} · ${message}`);
  state.loading = false;
  state.playing = false;
  restoreEngineDot();
  updateDeck();
  refreshActive();
  if (skippedInRow < state.queue.length - 1 && advanceQueue()) {
    skippedInRow++;
    toast(`SKIPPED · ${errText(t.title)}`);
  } else {
    toast(`SIGNAL LOST · ${errText(message)}`);
  }
}

function seekRatio(r) {
  const t = current();
  if (!t) return;
  state.position = t.duration * Math.min(1, Math.max(0, r));
  lastSeekAt = performance.now();
  Player?.seek(state.position);
  updateProgress();
}

// 이동 직후 잠깐은 mpv 의 예전 위치 알림을 무시 (바가 튀지 않게)
let lastSeekAt = 0;
function onPlayerTime(pos) {
  if (performance.now() - lastSeekAt < 600 || state.loading) return;
  state.position = pos;
  updateProgress();
}

// 가짜 재생 시계 (미리보기 전용) — 창이 숨으면 타이머가 느려지므로 실제 흐른 시간으로 계산
let lastTick = performance.now();
if (!REAL) setInterval(() => {
  const now = performance.now();
  const dt = (now - lastTick) / 1000;
  lastTick = now;
  const t = current();
  if (!state.playing || !t) return;
  state.position += dt;
  if (state.position >= t.duration) {
    state.position = t.duration;
    onTrackEnd();
  }
  updateProgress();
}, 250);

/* ───────── 하단 덱 ───────── */
function updateDeck() {
  const t = current();
  const label = $('#deck-label');
  if (!t) { label.textContent = 'STANDBY'; label.classList.add('idle'); }
  else if (state.loading) { label.textContent = 'ACQUIRING SIGNAL...'; label.classList.remove('idle'); }
  else if (state.playing) { label.textContent = 'NOW TRANSMITTING'; label.classList.remove('idle'); }
  else { label.textContent = 'SIGNAL PAUSED'; label.classList.add('idle'); }
  setDeckTitle(t);

  const pb = $('#btn-play');
  pb.classList.toggle('is-playing', state.playing);
  pb.classList.toggle('is-loading', state.loading);
  const rb = $('#btn-repeat');
  rb.classList.toggle('on', state.repeat !== 'off');
  rb.classList.toggle('one', state.repeat === 'one');
  rb.title = { off: '반복: 꺼짐 (R)', all: '반복: 목록 전체 (R)', one: '반복: 한 곡 (R)' }[state.repeat];
  $('#btn-like').classList.toggle('on', !!t && isLiked(t));

  document.title = t ? `${t.title} — AFTERLIFE` : 'AFTERLIFE';
  updateTrayTooltip();
  updateProgress();
}

/* 제목 해독 효과: 곡이 바뀌면 깨진 문자 → 왼쪽부터 한 글자씩 제자리로 */
const SCRAMBLE_CHARS = 'ABCDEFGHJKLMNPQRSTUVWXYZ0123456789#%&*+=/\\<>[]{}?!';
const DECODE_MS = 520;
let decodeFrame = 0;
let deckTitleId = null;

function setDeckTitle(t) {
  const el = $('#deck-title');
  const id = t ? t.id : null;
  if (id === deckTitleId) return; // 같은 곡이면 그대로
  deckTitleId = id;
  cancelAnimationFrame(decodeFrame);
  if (!t || document.hidden) return setMarquee(el, t ? t.title : 'NO SIGNAL');
  // 해독 효과가 끝나면 긴 제목은 흐르기 시작
  decodeText(el, t.title.toUpperCase(), () => setMarquee(el, t.title));
}

function decodeText(el, target, onDone) {
  el.classList.remove('marquee');
  const start = performance.now();
  const step = now => {
    const p = Math.min(1, (now - start) / DECODE_MS);
    const fixed = Math.floor(p * target.length);
    let out = target.slice(0, fixed);
    for (let i = fixed; i < target.length; i++) {
      out += target[i] === ' ' ? ' ' : SCRAMBLE_CHARS[(Math.random() * SCRAMBLE_CHARS.length) | 0];
    }
    el.textContent = out;
    if (p < 1) decodeFrame = requestAnimationFrame(step);
    else onDone?.();
  };
  decodeFrame = requestAnimationFrame(step);
}

// 트레이 아이콘에 마우스를 올리면 지금 곡이 보이게
let lastTooltip = '';
function updateTrayTooltip() {
  if (!window.__TAURI__) return;
  const t = current();
  const status = !t ? 'STANDBY' : state.loading ? 'LOADING' : state.playing ? 'PLAYING' : 'PAUSED';
  const text = t ? `AFTERLIFE · ${status}\n${t.title.toUpperCase()}` : 'AFTERLIFE · STANDBY';
  if (text === lastTooltip) return;
  lastTooltip = text;
  window.__TAURI__.core.invoke('set_tray_tooltip', { text });
}

function updateProgress() {
  const t = current();
  const dur = t ? t.duration : 0;
  $('#progress-fill').style.width = (dur ? (state.position / dur) * 100 : 0) + '%';
  $('#deck-time').textContent = `${fmt(state.position)} / ${fmt(dur)}`;
}

function updateVolume() {
  const input = $('#vol-input');
  if (document.activeElement !== input) input.value = state.volume.toFixed(1);
  $('#volume').classList.toggle('muted', state.muted);
  Player?.volume(engineVolume()); // 표시값 × 0.75
}

function setVolume(v) {
  state.volume = clampVolume(v);
  state.muted = false;
  updateVolume();
}

function commitVolumeInput() {
  const input = $('#vol-input');
  const v = parseFloat(input.value);
  if (Number.isFinite(v)) setVolume(v);
  input.value = state.volume.toFixed(1);
}

function toggleMute() {
  state.muted = !state.muted;
  updateVolume();
}

/* ───────── 팝오버 메뉴 ───────── */
const pop = $('#popover');
let popAnchor = null;

function openPopover(anchor, build, align = 'right') {
  closePopover();
  popAnchor = anchor;
  pop.dataset.align = align;
  anchor.classList.add('open');
  pop.innerHTML = '';
  build(pop);
  pop.hidden = false;
  placePopover();
}

function placePopover() {
  if (!popAnchor) return;
  const r = popAnchor.getBoundingClientRect();
  const pw = pop.offsetWidth, ph = pop.offsetHeight;
  // 버튼 끝(오른쪽 또는 왼쪽)에 맞춰 아래로, 공간이 없으면 위로
  const want = pop.dataset.align === 'left' ? r.left : r.right - pw;
  const left = Math.max(8, Math.min(want, innerWidth - pw - 8));
  const top = r.bottom + 4 + ph > innerHeight - 8 ? r.top - ph - 4 : r.bottom + 4;
  pop.style.left = left + 'px';
  pop.style.top = Math.max(8, top) + 'px';
}

function closePopover() {
  pop.hidden = true;
  popAnchor?.classList.remove('open');
  popAnchor = null;
}

function popEl(tag, cls, html) {
  const n = document.createElement(tag);
  n.className = cls;
  if (html != null) n.innerHTML = html;
  return n;
}

function popItem(name, { mark = '', count, current = false, onClick } = {}) {
  const b = popEl('button', `pop-item${current ? ' current' : ''}`,
    `<span class="mark">${mark}</span><span class="name">${escapeHtml(name)}</span>${count != null ? `<span class="cnt">${pad2(count)}</span>` : ''}`);
  b.onclick = onClick;
  return b;
}

// "+ NEW PLAYLIST" → 그 자리에서 이름 입력
function newPlaylistItem(onCreate) {
  const item = popItem('New playlist', { mark: '+' });
  item.onclick = () => {
    const input = popEl('input', 'pop-input');
    input.placeholder = 'PLAYLIST NAME';
    input.maxLength = 40;
    input.onkeydown = e => {
      e.stopPropagation();
      if (e.key === 'Enter' && input.value.trim()) { onCreate(input.value); closePopover(); }
      else if (e.key === 'Escape') closePopover();
    };
    item.replaceWith(input);
    placePopover();
    input.focus();
  };
  return item;
}

// 곡 옆 + 버튼: 플레이리스트에 넣기 / 빼기 (체크 표시 = 이미 들어 있음)
function openPlaylistMenu(anchor, t) {
  openPopover(anchor, p => {
    p.append(popEl('div', 'pop-label', 'ADD TO PLAYLIST'));
    for (const pl of library.playlists) {
      p.append(popItem(pl.name, {
        mark: hasTrack(pl.tracks, t) ? '✓' : '',
        count: pl.tracks.length,
        onClick: () => { closePopover(); togglePlaylistTrack(pl, t); },
      }));
    }
    if (library.playlists.length) p.append(popEl('div', 'pop-sep'));
    p.append(newPlaylistItem(name => {
      const pl = createPlaylist(name, t);
      toast(`ADDED TO · ${pl.name.toUpperCase()}`);
    }));
  });
}

document.addEventListener('pointerdown', e => {
  if (!pop.hidden && !pop.contains(e.target) && !popAnchor?.contains(e.target)) closePopover();
});
$('#track-list').addEventListener('scroll', closePopover);
window.addEventListener('resize', closePopover);

/* ───────── 옆 메뉴 (SEARCH · PLAYLIST · LIKED · SETTINGS) ───────── */
let playlistsExpanded = false;

function setDrawer(open) {
  closePopover();
  $('.terminal').classList.toggle('drawer-open', open);
  $('#btn-menu').title = open ? '메뉴 닫기' : '메뉴 열기';
  if (open) renderDrawer();
}
const drawerOpen = () => $('.terminal').classList.contains('drawer-open');

function renderDrawer() {
  $('#nav-pl-cnt').textContent = pad2(library.playlists.length);
  $('#nav-liked-cnt').textContent = pad2(library.liked.length);
  $('#nav-history-cnt').textContent = pad2(library.history.length);
  const onPlaylist = !!findPlaylist(state.source);
  document.querySelectorAll('.nav-item').forEach(b => {
    const nav = b.dataset.nav;
    b.classList.toggle('current',
      (nav === 'search' && state.source === 'signals') ||
      (nav === 'playlist' && onPlaylist) ||
      nav === state.source);
  });
  $('.nav-item[data-nav="playlist"]').classList.toggle('expanded', playlistsExpanded);

  const sub = $('#nav-playlists');
  sub.classList.toggle('open', playlistsExpanded);
  sub.innerHTML = '';
  for (const pl of library.playlists) {
    const b = popEl('button', `nav-sub-item${state.source === pl.id ? ' current' : ''}`,
      `<span class="name">${escapeHtml(pl.name)}</span><span class="cnt">${pad2(pl.tracks.length)}</span><span class="del" title="삭제 (두 번 클릭)">×</span>`);
    b.onclick = () => { setSource(pl.id); setDrawer(false); };
    // × 두 번 눌러야 삭제
    const del = b.querySelector('.del');
    del.onclick = e => {
      e.stopPropagation();
      if (del.classList.contains('armed')) { deletePlaylist(pl); renderDrawer(); return; }
      del.classList.add('armed');
      del.textContent = 'DEL?';
      del.style.width = 'auto';
    };
    sub.append(b);
  }
  // 새 플레이리스트 (그 자리에서 이름 입력)
  const add = popEl('button', 'nav-sub-item', '<span class="name">+ NEW PLAYLIST</span>');
  add.onclick = () => {
    const input = popEl('input', 'pop-input');
    input.placeholder = 'PLAYLIST NAME';
    input.maxLength = 40;
    input.onkeydown = e => {
      e.stopPropagation();
      if (e.key === 'Enter' && input.value.trim()) { setSource(createPlaylist(input.value).id); setDrawer(false); }
      else if (e.key === 'Escape') renderDrawer();
    };
    add.replaceWith(input);
    input.focus();
  };
  sub.append(add);
}

function onNav(nav) {
  if (nav === 'playlist') {
    playlistsExpanded = !playlistsExpanded;
    return renderDrawer();
  }
  setDrawer(false);
  if (nav === 'search') {
    setSource('signals');
    $('#scan-input').focus();
    $('#scan-input').select();
  } else {
    setSource(nav); // 'liked' | 'settings'
  }
}

/* ───────── Wiring ───────── */
$('#scan-form').onsubmit = e => { e.preventDefault(); scan($('#scan-input').value); };
$('#btn-menu').onclick = () => setDrawer(!drawerOpen());
$('#drawer-backdrop').onclick = () => setDrawer(false);
document.querySelectorAll('.nav-item').forEach(b => (b.onclick = () => onNav(b.dataset.nav)));
$('#btn-like').onclick = () => toggleLike(current());
// PLAY ALL: 지금 보는 목록을 1번 곡부터 이어 재생
$('#btn-play-all').onclick = () => {
  const list = visibleList();
  if (list.length) playFrom(list, 0);
};
$('#btn-play').onclick = togglePlay;
$('#btn-repeat').onclick = toggleRepeat;
$('#btn-mute').onclick = toggleMute;

// 진행 바: 클릭 / 드래그로 이동
const progress = $('#progress');
progress.addEventListener('pointerdown', e => {
  progress.setPointerCapture(e.pointerId);
  const move = ev => {
    const r = progress.getBoundingClientRect();
    seekRatio((ev.clientX - r.left) / r.width);
  };
  move(e);
  progress.onpointermove = move;
  progress.onpointerup = () => { progress.onpointermove = null; };
});

// 볼륨: 휠 ±1, Shift+휠 ±0.1, 숫자 직접 입력
const volInput = $('#vol-input');
$('#volume').addEventListener('wheel', e => {
  e.preventDefault();
  const step = e.shiftKey ? 0.1 : 1;
  const dir = (e.deltaY || e.deltaX) < 0 ? 1 : -1;
  setVolume(state.volume + dir * step);
  if (document.activeElement === volInput) volInput.value = state.volume.toFixed(1);
}, { passive: false });
volInput.addEventListener('focus', () => volInput.select());
volInput.addEventListener('blur', commitVolumeInput);
volInput.addEventListener('input', () => {
  // 숫자와 소수점 한 자리까지만 남김
  const cleaned = volInput.value
    .replace(/[^0-9.]/g, '')
    .replace(/(\..*)\./g, '$1')
    .replace(/(\.\d).+/, '$1');
  if (cleaned !== volInput.value) volInput.value = cleaned;
});
volInput.addEventListener('keydown', e => {
  e.stopPropagation();
  if (e.key === 'Enter') { commitVolumeInput(); volInput.blur(); }
  else if (e.key === 'Escape') { volInput.value = state.volume.toFixed(1); volInput.blur(); }
  else if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
    e.preventDefault();
    setVolume(state.volume + (e.key === 'ArrowUp' ? 1 : -1) * (e.shiftKey ? 0.1 : 1));
    volInput.value = state.volume.toFixed(1);
    volInput.select();
  }
});

document.addEventListener('keydown', e => {
  const typing = e.target.matches('input');
  if ((e.ctrlKey && e.key.toLowerCase() === 'k') || (!typing && e.key === '/')) {
    e.preventDefault();
    $('#scan-input').focus();
    $('#scan-input').select();
    return;
  }
  if (e.key === 'Escape') {
    if (!pop.hidden) return closePopover();
    if (drawerOpen()) return setDrawer(false);
    if (typing) e.target.blur();
    if (state.query) clearScan();
    return;
  }
  if (typing) return;
  const t = current();
  if (e.code === 'Space') { e.preventDefault(); togglePlay(); }
  else if (e.key === 'ArrowRight' && t) seekRatio((state.position + 5) / t.duration);
  else if (e.key === 'ArrowLeft' && t) seekRatio((state.position - 5) / t.duration);
  else if (e.key === 'ArrowUp') { e.preventDefault(); setVolume(state.volume + (e.shiftKey ? 0.1 : 1)); }
  else if (e.key === 'ArrowDown') { e.preventDefault(); setVolume(state.volume - (e.shiftKey ? 0.1 : 1)); }
  else if (e.key.toLowerCase() === 'm') toggleMute();
  else if (e.key.toLowerCase() === 'r') toggleRepeat();
  else if (e.key.toLowerCase() === 'l') toggleLike(t);
});

/* ───────── Tauri (창 컨트롤 · 엔진 상태) ───────── */
const tauri = window.__TAURI__;
if (tauri) {
  const win = tauri.window.getCurrentWindow();
  $('#win-min').onclick = () => win.minimize();
  $('#win-close').onclick = () => win.close(); // Rust 쪽에서 트레이로 숨김 처리
  // 백그라운드 재생: 창만 숨기고 재생은 계속 (트레이 아이콘으로 다시 열기)
  $('#btn-background').onclick = () => win.hide();

  // 트레이 메뉴 "재생 / 일시정지"
  tauri.event.listen('tray://toggle-play', () => togglePlay());

  invoke('engine_status').then(({ ytdlp, mpv }) => {
    engines.checked = true;
    tools.ytdlp.version = ytdlp;
    tools.mpv.version = mpv ? 'INSTALLED' : null;
    syncEngines();
    // 시작할 때마다 GitHub 확인: yt-dlp 는 최신 유지, libmpv 는 없으면 설치
    ensureTool('ytdlp');
    ensureTool('mpv');
  });

  // 다운로드 진행률 (1% 단위로만 글자 갱신)
  for (const [key, ev] of [['ytdlp', 'ytdlp://progress'], ['mpv', 'mpv://progress']]) {
    tauri.event.listen(ev, ({ payload }) => {
      const tl = tools[key];
      const p = payload.total ? payload.downloaded / payload.total : 0;
      if (Math.floor(p * 100) === Math.floor(tl.progress * 100)) return;
      tl.progress = p;
      const v = $(`#${key}-value`);
      if (v) v.textContent = toolText(tl)[0];
    });
  }

  // libmpv 이벤트 (Rust: player.rs)
  tauri.event.listen('player://loaded', onLoaded);
  tauri.event.listen('player://time', ({ payload }) => onPlayerTime(payload.pos));
  tauri.event.listen('player://duration', ({ payload }) => {
    const t = current();
    if (!t || !(payload.duration > 0)) return;
    t.duration = payload.duration;
    updateProgress();
  });
  tauri.event.listen('player://ended', () => current() && onTrackEnd());
  tauri.event.listen('player://error', ({ payload }) => {
    const t = current();
    if (t && state.loading) return onLoadFailed(t, `mpv: ${payload.message}`); // 여는 중 실패 → 재시도
    plog(`mpv 재생 중 끊김: ${t?.title} · ${payload.message}`);
    state.loading = false;
    state.playing = false;
    restoreEngineDot();
    updateDeck();
    refreshActive();
    toast(`SIGNAL LOST · ${errText(payload.message)}`);
  });
} else {
  // 브라우저 미리보기
  $('#win-ctrls').style.display = 'none';
  $('#btn-background').onclick = () => toast('BACKGROUND · 앱에서만 동작');
  $('#engine-dot').title = '브라우저 미리보기 (엔진 없음)';
}

renderList();
updateDeck();
updateVolume();
