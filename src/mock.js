// 임시 데이터 & 가짜 서비스.
// Rust 백엔드(yt-dlp + libmpv)가 붙으면 MockService 대신 Tauri invoke 호출로 교체한다.
// Track 모양은 yt-dlp -J 결과에서 뽑을 필드에 맞춰 둔다.
//   { id, title, artist, duration(초), thumbnail(url|null), source }

const MOCK_TRACKS = [
  { id: 's1', title: 'Night City Drive', artist: 'Neon Static', duration: 222 },
  { id: 's2', title: 'Resist and Disorder', artist: 'Chrome Saints', duration: 256 },
  { id: 's3', title: 'Afterlife', artist: 'Silver Ghost', duration: 208 },
  { id: 's4', title: 'Neon Requiem', artist: 'Data Choir', duration: 302 },
  { id: 's5', title: 'Ghost in the Signal', artist: 'Null Frequency', duration: 177 },
  { id: 's6', title: 'Chrome Heart', artist: 'Kairo', duration: 231 },
  { id: 's7', title: 'Last Transmission', artist: 'Mira Vex', duration: 265 },
  { id: 's8', title: 'Black Ice Protocol', artist: 'Data Choir', duration: 241 },
  { id: 's9', title: 'Soulkiller', artist: 'Silver Ghost', duration: 219 },
  { id: 's10', title: 'Static Halo', artist: 'Null Frequency', duration: 196 },
  { id: 's11', title: 'Red Line Express', artist: 'Chrome Saints', duration: 248 },
  { id: 's12', title: 'Echoes After Midnight', artist: 'Neon Static', duration: 283 },
].map(t => ({ ...t, thumbnail: null, source: 'youtube' }));

const MockService = {
  // yt-dlp "ytsearch20:<query>" 자리
  async search(query) {
    await delay(500 + Math.random() * 500);
    const q = query.trim().toLowerCase();
    return MOCK_TRACKS
      .filter(t => t.title.toLowerCase().includes(q) || t.artist.toLowerCase().includes(q))
      .map(t => ({ ...t }));
  },

  // yt-dlp로 오디오 URL 추출 → libmpv loadfile 자리
  async resolveStream(track) {
    await delay(400 + Math.random() * 600);
    return { url: null, duration: track.duration };
  },
};

function delay(ms) { return new Promise(r => setTimeout(r, ms)); }
