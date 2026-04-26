# jellyfin-rs

A lightweight Jellyfin-compatible media server written in Rust. Designed for direct-play streaming with minimal resource usage.

## Features

- **Jellyfin API compatible** — Works with Jellyfin clients including Tsukimi
- **Direct-play streaming** — Video, audio, and subtitle streaming with HTTP range support
- **Library scanning** — Scans media directories with `.nfo` metadata and sidecar art/subtitle detection
- **ffprobe integration** — Optional media probing for codec, resolution, and duration
- **TMDb integration** — Movie metadata search, details, and artwork (requires API key)
- **User management** — Multiple users, authentication tokens, and playback tracking
- **Image management** — Upload, serve (with ETag/304), and delete artwork
- **Playback sessions** — Track active sessions with configurable timeout
- **Activity logging** — Audit log for logins, scans, and metadata changes
- **SQLite by default** — PostgreSQL supported via `JELLYFIN_RS_DATABASE_URL`

## Quick Start

```bash
# Set required environment variables
$env:JELLYFIN_RS_MEDIA_DIRS = "D:\Movies;D:\TV Shows"
$env:JELLYFIN_RS_USER = "admin"
$env:JELLYFIN_RS_PASSWORD = "your-password"

# Optional: TMDb API key for metadata
$env:JELLYFIN_RS_TMDB_API_KEY = "your-tmdb-key"

cargo run --release
```

Server starts on `http://127.0.0.1:8096` by default. Connect any Jellyfin client using the admin credentials.

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `JELLYFIN_RS_HOST` | `127.0.0.1` | Listen address |
| `JELLYFIN_RS_PORT` | `8096` | Listen port |
| `JELLYFIN_RS_DATABASE_URL` | `sqlite://jellyfin-rs.db` | Database connection |
| `JELLYFIN_RS_MEDIA_DIRS` | (none) | Semicolon-separated media directories |
| `JELLYFIN_RS_SCAN_ON_STARTUP` | `true` | Scan library on startup |
| `JELLYFIN_RS_USER` | `tsukimi` | Default admin username |
| `JELLYFIN_RS_PASSWORD` | `tsukimi` | Default admin password |
| `JELLYFIN_RS_TMDB_API_KEY` | (none) | TMDb API key for metadata |
| `JELLYFIN_RS_FFPROBE_PATH` | (system PATH) | Path to ffprobe binary |
| `JELLYFIN_RS_SESSION_TIMEOUT_SECONDS` | `120` | Inactive session timeout |

## API Coverage

### Implemented Endpoints (75+)

**System:** Info, Activity Log, Scheduled Tasks, Shutdown
**Library:** Virtual Folders CRUD, Media Folders, Refresh/Scan
**Users:** Authenticate, CRUD, Password, Avatar
**Items:** Browse, Search, Filter, Latest, Resume, Similar, Counts
**TV:** Seasons, Episodes, Next Up
**Playback:** PlaybackInfo, Direct Stream, Progress Tracking
**Images:** Upload, Serve (ETag/304), Delete, Remote (TMDb)
**Metadata:** Update, Remote Search/Apply, Reset, NFO Parser
**Sessions:** Active Sessions, Capabilities, Timeout
**Filters:** Genres, Tags, Persons, Studios, Years, Containers, Codecs
**Preferences:** Display Preferences Save/Load
**Ratings:** Like/Dislike, Numeric Rating

### Not Yet Implemented

- Transcoding / HLS streaming
- Live TV / DVR
- Plugins system
- SyncPlay
- Collections / Playlists
- TVDb / IMDb / MusicBrainz providers

## Building

Requires Rust 1.85+ (edition 2024).

```bash
cargo build --release
```

## License

MIT
