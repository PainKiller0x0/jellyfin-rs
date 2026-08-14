# jellyfin-rs

`jellyfin-rs` 是一个使用 Rust 编写的轻量级 Jellyfin/Emby 兼容媒体服务器。它优先服务于直播放流、低资源占用、中文媒体库体验和常用客户端兼容，适合在 NAS、家庭服务器或开发环境里作为 Jellyfin API 兼容服务使用。

项目仍在快速迭代中。当前重点是媒体库管理、直播放流、播放进度、封面/元数据、管理后台和常用客户端 API 覆盖；转码、HLS、直播电视等完整 Jellyfin 能力尚未实现。

## 功能亮点

- Jellyfin/Emby API 兼容：支持登录、用户、媒体库视图、媒体列表、详情、搜索、过滤、收藏、播放状态、会话、图片和元数据管理等常用接口。
- 直播放流：视频、音频、外挂字幕直出，支持 HTTP Range 和 `206 Partial Content`，适配常见 Jellyfin/Emby 客户端的直接播放路径。
- 媒体库扫描：支持电影、剧集、季、集、混合内容识别，读取 `.nfo`，识别同目录海报/背景图/字幕，并通过 `ffprobe` 提取媒体流信息。
- 元数据增强：支持 TMDb API Key、TMDb 代理、豆瓣 Cookie 配置，可补全影片、剧集、剧集分集、人物和远程图片信息。
- STRM 与远程媒体：支持 `.strm` 文件，能解析本地路径、相对路径、`file://`、HTTP/HTTPS 目标并用于分类和直播放流。
- 中文体验：包含简繁转换、中文搜索增强、可选拼音排序等面向中文媒体库的能力。
- 管理后台：内置 Vue 3 管理界面，可管理媒体库、用户、计划任务、服务器设置、API Key、播放统计、活动日志和请求日志。
- 离线播放地图：内置 `ip2region` IPv4 数据库，用于管理后台播放地域统计，默认不需要把 IP 发给外部地理位置服务。

## 当前边界

已支持的主要场景：

- 使用管理员账号登录兼容客户端。
- 创建媒体库并扫描本地媒体目录。
- 浏览电影、剧集、合集、播放列表、最近添加、续播和搜索结果。
- 直接播放本地文件或 STRM 指向的远程文件。
- 同步播放进度、收藏、已播放状态和活跃会话。
- 上传、删除、下载本地或远程封面图片。
- 通过管理后台维护服务器名称、TMDb、豆瓣、API Key 和用户。

暂未实现或仅提供占位响应的能力：

- 转码、HLS 和复杂码率自适应。
- 直播电视、DVR、SyncPlay、完整插件系统。
- Quick Connect 的完整生产级流程。
- TVDb、IMDb、MusicBrainz 等完整外部提供者。
- 完整缺失剧集检测。

## 快速开始

### 依赖

- Rust 1.97.1 或更新版本
- PostgreSQL
- `ffmpeg`/`ffprobe`，用于媒体探测和未来增强能力
- Node.js、pnpm 11.15.1，用于构建管理后台

### 本地运行

先准备 PostgreSQL。应用会尝试自动创建 `JELLYFIN_RS_DATABASE_URL` 指向的数据库，但连接账号需要有连接维护库和创建数据库的权限。

```bash
export JELLYFIN_RS_DATABASE_URL="postgresql://postgres:postgres@127.0.0.1:5432/jellyfin_rs"
export JELLYFIN_RS_USER="admin"
export JELLYFIN_RS_PASSWORD="change-me"
export RUST_LOG="info"

pnpm --dir admin install
pnpm --dir admin build

cargo run --release
```

服务默认监听：

- API: `http://127.0.0.1:8096`
- 管理后台: `http://127.0.0.1:8096/admin`

首次启动后，用 `JELLYFIN_RS_USER` 和 `JELLYFIN_RS_PASSWORD` 登录管理后台，在“媒体库”里创建媒体库并添加路径，然后触发扫描。

### 管理后台开发模式

```bash
pnpm --dir admin install
pnpm --dir admin dev
```

开发环境默认通过 `admin/.env.development` 中的 `VITE_JELLYFIN_API_BASE=http://127.0.0.1:8096` 访问后端。

## Docker

仓库提供 `Dockerfile` 和 `docker-compose.yml`。镜像构建会同时构建 Rust 后端和管理后台。

```bash
docker compose up --build
```

默认 compose 使用 host 网络，并假设 PostgreSQL 在宿主机 `127.0.0.1:5432` 可访问。常用覆盖项：

```bash
JELLYFIN_RS_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/jellyfin_rs \
JELLYFIN_RS_USER=admin \
JELLYFIN_RS_PASSWORD=change-me \
JELLYFIN_RS_MEDIA_ROOT=/path/to/media \
docker compose up --build
```

容器内默认把媒体目录挂载到 `/media`，仍需在管理后台里把 `/media` 添加为媒体库路径。

## 配置

常用环境变量：

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `JELLYFIN_RS_HOST` | `127.0.0.1` | 监听地址，Docker 中默认设为 `0.0.0.0` |
| `JELLYFIN_RS_PORT` | `8096` | 监听端口 |
| `JELLYFIN_RS_PUBLIC_URL` | 按请求 Host 推断 | 反向代理或 Docker 场景下返回给客户端的外部访问地址 |
| `JELLYFIN_RS_DATABASE_URL` | `postgresql://postgres:postgres@127.0.0.1:5432/jellyfin_rs` | PostgreSQL 连接地址 |
| `JELLYFIN_RS_USER` | `tsukimi` | 启动时创建的默认管理员用户名 |
| `JELLYFIN_RS_PASSWORD` | 同用户名 | 启动时创建的默认管理员密码 |
| `JELLYFIN_RS_SCAN_ON_STARTUP` | `true` | 启动后是否自动扫描已配置的媒体库路径 |
| `JELLYFIN_RS_FFPROBE_PATH` | `ffprobe` | 媒体流探测工具路径 |
| `JELLYFIN_RS_FFPROBE_ANALYZE_DURATION` | `30000000` | ffprobe `-analyzeduration` 参数，设为 `0` 可不传 |
| `JELLYFIN_RS_FFPROBE_PROBE_SIZE` | `100000000` | ffprobe `-probesize` 参数，设为 `0` 可不传 |
| `JELLYFIN_RS_WATCH_DEBOUNCE_SECONDS` | `10` | 文件变化触发扫描前的防抖秒数 |
| `JELLYFIN_RS_WATCH_POLL_SECONDS` | `60` | 文件监听轮询兜底间隔，设为 `0` 可关闭轮询 |
| `JELLYFIN_RS_SCAN_ROOT_CONCURRENCY` | 按 CPU 自动计算 | 同时扫描的媒体库根目录数量，范围 `1-4` |
| `JELLYFIN_RS_INGEST_CONCURRENCY` | 按 CPU 自动计算 | 媒体入库并发数，范围 `1-4` |
| `JELLYFIN_RS_METADATA_CONCURRENCY` | 按 CPU 自动计算 | TMDb/豆瓣元数据补全并发数，范围 `1-4` |
| `JELLYFIN_RS_MEDIA_PROBE_CONCURRENCY` | 按 CPU 自动计算 | 媒体流探测并发数，范围 `1-4` |
| `JELLYFIN_RS_SESSION_TIMEOUT_SECONDS` | `120` | 播放会话超时秒数 |
| `JELLYFIN_RS_MAX_WATCH_DELTA_SECONDS` | `43200` | 播放进度上报允许的最大时间跳变，单位秒 |
| `JELLYFIN_RS_PROXY` | 无 | 通用外部 HTTP 请求代理，优先级高于标准代理变量 |
| `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` | 无 | 标准代理变量，包括 TMDb 官方与 TMDb 反代请求 |
| `NO_PROXY` | `localhost,127.0.0.1,::1` | 标准代理绕过列表 |
| `JELLYFIN_RS_NO_PROXY` | 无 | 禁用通用代理 |

媒体入库的数据库连接池和队列容量会根据 CPU 核心数自动计算；扫描、入库、媒体探测和元数据补全并发可通过上表环境变量在合理范围内覆盖，以便按 VPS 配置调节。
| `JELLYFIN_RS_TMDB_API_KEY` | 无 | 启动时读取的 TMDb API Key，也可在后台设置 |
| `JELLYFIN_RS_IP2REGION_V4_XDB` | 内置数据库 | 自定义 IPv4 离线归属地库路径 |

STRM Assistant 相关配置既可通过数据库中的 `sa.*` 设置保存，也可用环境变量覆盖。常用变量包括：

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `JELLYFIN_RS_SA_ENABLED` | `true` | 总开关 |
| `JELLYFIN_RS_SA_STRM_ENABLED` | `true` | STRM 相关能力开关 |
| `JELLYFIN_RS_SA_FFMPEG_PATH` | `ffmpeg` | ffmpeg 路径 |
| `JELLYFIN_RS_SA_FFPROBE_PATH` | `ffprobe` | ffprobe 路径 |
| `JELLYFIN_RS_SA_FPCALC_PATH` | `fpcalc` | 音频指纹工具路径 |
| `JELLYFIN_RS_SA_INTRO_SKIP_ENABLED` | `true` | 片头/片尾检测能力开关 |
| `JELLYFIN_RS_SA_FINGERPRINT_ENABLED` | `false` | 指纹检测开关 |
| `JELLYFIN_RS_SA_THUMBNAIL_ENABLED` | `false` | 视频缩略图生成开关 |
| `JELLYFIN_RS_SA_MEDIAINFO_ENABLED` | `false` | MediaInfo JSON 提取开关 |
| `JELLYFIN_RS_SA_CHINESE_CONVERT` | `true` | 简繁转换 |
| `JELLYFIN_RS_SA_CHINESE_SEARCH` | `true` | 中文搜索增强 |
| `JELLYFIN_RS_SA_PINYIN_SORTING` | `false` | 拼音排序 |
| `JELLYFIN_RS_SA_MERGE_ENABLED` | `false` | 多版本合并 |
| `JELLYFIN_RS_SA_ENHANCED_SUBTITLE_SCAN` | `true` | 增强字幕扫描 |

## 媒体库约定

推荐目录结构：

```text
/media
  /Movies
    /Inception (2010)
      Inception (2010).mkv
      poster.jpg
      fanart.jpg
      Inception (2010).zh-CN.srt
  /TV
    /Show Name (2024)
      /Season 01
        Show Name - S01E01.mkv
```

支持的常见旁路文件：

- 元数据：`.nfo`
- 图片：`poster`、`fanart`、`backdrop`、`thumb` 等同目录图片
- 字幕：`srt`、`ass`、`ssa`、`vtt`、`sub`
- STRM：文件内容第一条非空、非注释行作为目标地址

## 常用 API 范围

项目覆盖的 API 数量较多，完整路由以 `src/jellyfin/routes.rs` 为准。常用范围包括：

- System: `/System/Info`、`/System/Info/Public`、`/System/ActivityLog/Entries`、`/ScheduledTasks`
- Auth/Users: `/Users/AuthenticateByName`、`/Users`、`/Users/Me`、`/Auth/Keys`
- Library: `/Library/VirtualFolders`、`/Library/VirtualFolders/Paths`、`/Library/Refresh`
- Browsing: `/Users/{userId}/Views`、`/Users/{userId}/Items`、`/Items/Counts`、`/Search/Hints`
- Playback: `/Items/{itemId}/PlaybackInfo`、`/Videos/{itemId}/stream.{container}`、`/Audio/{itemId}/universal`
- Sessions: `/Sessions`、`/Sessions/Playing`、`/Sessions/Playing/Progress`、`/Sessions/Playing/Stopped`
- Images: `/Items/{itemId}/Images`、`/Items/{itemId}/RemoteImages`
- Metadata: `/Items/{itemId}`、`/Items/RemoteSearch/{type}`、`/Items/RemoteSearch/Apply/{itemId}`
- Collections/Playlists: `/Collections`、`/Playlists`

同时支持 `/emby/...` 前缀，便于 Emby 兼容客户端访问。

## 开发与验证

构建后端：

```bash
cargo build --release
```

运行 Rust 测试：

```bash
cargo test
```

构建管理后台：

```bash
pnpm --dir admin build
```

OpenAPI 兼容性测试位于 `tests/openapi_contract`。先启动本服务，再运行：

```bash
uv run --project tests/openapi_contract pytest tests/openapi_contract
```

默认测试连接：

- `EMBY_BASE_URL=http://127.0.0.1:8096`
- `EMBY_USERNAME=admin`
- `EMBY_PASSWORD=123456`
- `EMBY_OPENAPI=docs/emby-openapi.json`

## 致谢

本项目深受以下项目启发：

- [Jellyfin](https://github.com/jellyfin/jellyfin)：自由开源媒体系统，提供 API 规范与参考实现。
- [Tsukimi](https://github.com/tsukinaha/tsukimi)：GTK Jellyfin 客户端，本项目优先参考并适配其常用调用路径。
- [ip2region](https://github.com/lionsoul2014/ip2region)：离线 IP 归属地数据库，用于管理后台播放地域统计。

## License

GPL-2.0-only
