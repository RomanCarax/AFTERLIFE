# AFTERLIFE

사이버펑크 "오디오 터미널" 느낌의 Windows 음악 플레이어.
YouTube에서 검색해서 오디오만 재생합니다.

- **Rust + Tauri v2** 데스크톱 앱 (창 420×650 고정)
- **yt-dlp**: 검색 · 오디오 주소 추출
- **libmpv**: 재생

yt-dlp와 libmpv는 설치할 필요가 없습니다(포터블). 처음 실행할 때 앱이 GitHub 공식 릴리스에서 직접 받아
SHA-256 해시를 확인한 뒤 `%LOCALAPPDATA%\com.afterlife.player\bin` 에 둡니다.
yt-dlp는 실행할 때마다 최신 버전으로 맞춰지고, libmpv는 SETTINGS의 `[ CHECK ]`로 업데이트합니다.

## 기능

- 검색(최대 10곡) · YouTube 링크 바로 재생
- 재생 / 일시정지 · 한 곡 반복 · 위치 이동
- 볼륨 0.0 ~ 100.0 (소수점 한 자리, 기본 50.0, 표시 100 = 실제 75%)
- HISTORY · PLAYLIST · LIKED
- 트레이로 숨겨서 백그라운드 재생

## 실행

필요: Rust, [Tauri CLI](https://v2.tauri.app/) (`cargo install tauri-cli`)

```bash
cd src-tauri
cargo tauri dev
```

## 단축키

| 키 | 동작 |
|---|---|
| `Space` | 재생 / 일시정지 |
| `← →` | 5초 이동 |
| `↑ ↓` | 볼륨 ±1 (`Shift` ±0.1) |
| `M` · `R` · `L` | 음소거 · 반복 · 좋아요 |
| `Ctrl+K` | 검색 |

## 사용한 것

- 폰트: [Rajdhani](https://fonts.google.com/specimen/Rajdhani) (SIL Open Font License, `src/fonts/OFL.txt`)
- [yt-dlp](https://github.com/yt-dlp/yt-dlp), [mpv](https://mpv.io/) — 실행 중에 받아서 사용하며 이 저장소에는 포함하지 않습니다.
