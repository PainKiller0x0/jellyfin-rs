# jellyfin-rs

轻量级 Jellyfin 兼容媒体服务器，使用 Rust 编写。专注直播放流，资源占用极低。

A lightweight Jellyfin-compatible media server written in Rust. Designed for direct-play streaming with minimal resource usage.

## 致谢 / Acknowledgments

本项目深受以下开源项目启发，在此表示感谢：

This project is deeply inspired by and grateful to:

- **[Jellyfin](https://github.com/jellyfin/jellyfin)** — The Free Software Media System. 提供了完整的 API 规范和参考实现。
- **[Tsukimi](https://github.com/tsukinaha/tsukimi)** — 优秀的 GTK Jellyfin 客户端。本项目优先适配 Tsukimi 的 API 调用，感谢其清晰简洁的代码作为集成参考。

## 功能 / Features

- **Jellyfin API 兼容** — 可连接 Jellyfin 客户端（包括 Tsukimi）
- **直播放流** — 视频/音频/字幕直出，支持 HTTP Range 断点续传
- **媒体库扫描** — 自动扫描目录，解析 `.nfo` 元数据，识别同目录封面/字幕
- **ffprobe 探测** — 可选媒体流分析（编码/分辨率/时长）
- **TMDb 集成** — 影片元数据搜索、详情与海报（需 API Key）
- **用户管理** — 多用户、Token 认证、播放进度追踪
- **图片管理** — 上传/获取/删除封面海报，ETag/304 缓存
- **播放会话** — 活跃会话追踪，可配超时清理
- **活动日志** — 登录/扫描/编辑操作审计
- **PostgreSQL 存储** — 使用 `JELLYFIN_RS_DATABASE_URL` 配置数据库连接

## 快速开始 / Quick Start

```bash
# 设置环境变量
export JELLYFIN_RS_MEDIA_DIRS="/data/movies;/data/tvshows"
export JELLYFIN_RS_USER="admin"
export JELLYFIN_RS_PASSWORD="your-password"

# 可选：TMDb API Key 通过管理接口配置

cargo run --release
```

服务默认监听 `http://127.0.0.1:8096`。使用管理员账号连接任意 Jellyfin 客户端。

### 管理后台 / Admin Console

管理后台位于 `admin/`，基于 Vue 3、Vite、TypeScript、Pinia、Element Plus 和 UnoCSS 构建。

```bash
pnpm --dir admin install
pnpm --dir admin build
cargo run --release
```

构建后访问 `http://127.0.0.1:8096/admin`。开发模式可运行 `pnpm --dir admin dev`，默认代理到 `VITE_JELLYFIN_API_BASE`。

## 配置 / Configuration

| 环境变量 | 默认值 | 说明 |
|----------|--------|------|
| `JELLYFIN_RS_HOST` | `127.0.0.1` | 监听地址 |
| `JELLYFIN_RS_PORT` | `8096` | 监听端口 |
| `JELLYFIN_RS_DATABASE_URL` | `postgresql://postgres:postgres@127.0.0.1:5432/jellyfin_rs` | PostgreSQL 数据库连接 |
| `JELLYFIN_RS_MEDIA_DIRS` | (无) | 媒体目录，分号分隔 |
| `JELLYFIN_RS_SCAN_ON_STARTUP` | `true` | 启动时扫描媒体库 |
| `JELLYFIN_RS_USER` | `tsukimi` | 默认管理员用户名 |
| `JELLYFIN_RS_PASSWORD` | `tsukimi` | 默认管理员密码 |
| `JELLYFIN_RS_FFPROBE_PATH` | (系统 PATH) | ffprobe 路径 |
| `JELLYFIN_RS_SESSION_TIMEOUT_SECONDS` | `120` | 会话超时秒数 |

## API 覆盖 / API Coverage (80+ endpoints)

### 系统 / System

| Method | Endpoint | 说明 |
|--------|----------|------|
| GET | `/System/Info` | 服务器信息（名称/版本/OS） |
| GET | `/System/Info/Public` | 公开服务器信息 |
| GET | `/System/ActivityLog/Entries` | 活动日志（支持 hasUserId、分页） |
| POST | `/System/Shutdown` | 关闭服务器 |
| POST | `/System/Restart` | 重启服务器 |
| GET | `/ScheduledTasks` | 计划任务列表（含上次执行结果） |
| POST | `/ScheduledTasks/Running/{id}` | 触发计划任务 |

### 媒体库管理 / Library Management

| Method | Endpoint | 说明 |
|--------|----------|------|
| GET | `/Library/VirtualFolders` | 列出虚拟文件夹 |
| POST | `/Library/VirtualFolders` | 创建虚拟文件夹 |
| POST | `/Library/VirtualFolders/Paths` | 添加媒体路径 |
| DELETE | `/Library/VirtualFolders/Paths` | 删除媒体路径 |
| GET | `/Library/MediaFolders` | 获取媒体文件夹 |
| POST | `/Library/Refresh` | 全量扫描媒体库 |

### 用户 / Users

| Method | Endpoint | 说明 |
|--------|----------|------|
| POST | `/Users/AuthenticateByName` | 用户名+密码登录 |
| GET | `/Users` | 列出所有用户 |
| GET | `/Users/Me` | 获取当前用户信息 |
| POST | `/Users/New` | 创建新用户 |
| GET | `/Users/{userId}` | 获取指定用户 |
| DELETE | `/Users/{userId}` | 删除用户 |
| POST | `/Users/{userId}/Password` | 修改密码 |
| GET | `/Users/{userId}/Images/Primary` | 用户头像 |

### 媒体浏览 / Media Browsing

| Method | Endpoint | 说明 |
|--------|----------|------|
| GET | `/Users/{userId}/Views` | 获取用户媒体库视图 |
| GET | `/Users/{userId}/Items` | 媒体列表（支持 20+ 过滤参数） |
| GET | `/Users/{userId}/Items/Latest` | 最近添加 |
| GET | `/Users/{userId}/Items/Resume` | 续播列表 |
| GET | `/Users/{userId}/Items/{itemId}` | 媒体详情（含 ProviderIds/People/ImageTags） |
| GET | `/Items/{itemId}/Similar` | 相似媒体（共享类型匹配） |
| GET | `/Items/Counts` | 各库媒体数量统计 |

### 搜索与过滤 / Search & Filters

| Method | Endpoint | 说明 |
|--------|----------|------|
| GET | `/Search/Hints` | 搜索建议 |
| GET | `/Genres` | 类型列表 |
| GET | `/Tags` | 标签列表 |
| GET | `/Persons` | 人物列表 |
| GET | `/Studios` | 工作室列表 |
| GET | `/Years` | 年代列表 |
| GET | `/Containers` | 容器格式列表 |
| GET | `/VideoCodecs` | 视频编码列表 |
| GET | `/OfficialRatings` | 分级列表（暂无数据） |
| GET | `/ExtendedVideoTypes` | 扩展视频类型（暂无数据） |

支持的 Items 过滤参数：`SearchTerm` `IncludeItemTypes` `ExcludeItemTypes` `Filters`(IsPlayed/IsUnplayed/IsResumable/IsFavorite) `SortBy`(SortName/ProductionYear/Runtime/DatePlayed/DateCreated/Random/PlayCount) `SortOrder` `Years` `GenreIds` `TagIds` `PersonIds` `StudioIds` `Containers` `VideoCodecs` `MinWidth` `MaxWidth` `MediaTypes` `Recursive`

### 剧集 / TV Shows

| Method | Endpoint | 说明 |
|--------|----------|------|
| GET | `/Shows/{showId}/Seasons` | 获取季列表 |
| GET | `/Shows/{showId}/Episodes` | 获取集列表（支持 SeasonId） |
| GET | `/Shows/NextUp` | 待看下一集（未播放剧集） |
| GET | `/Shows/Missing` | 缺失剧集（暂无外部数据） |

### 播放 / Playback

| Method | Endpoint | 说明 |
|--------|----------|------|
| POST | `/Items/{itemId}/PlaybackInfo` | 获取播放信息（MediaSources/Streams） |
| GET/HEAD | `/Videos/{itemId}/stream.{container}` | 视频直出串流（HTTP Range/206） |
| GET/HEAD | `/Audio/{itemId}/universal` | 音频直出串流 |
| GET/HEAD | `/Videos/{itemId}/Subtitles/{idx}/Stream.{fmt}` | 外挂字幕串流 |
| GET | `/Items/{itemId}/Subtitles` | 字幕流列表 |
| POST | `/Users/{userId}/FavoriteItems/{itemId}` | 收藏 |
| POST | `/Users/{userId}/FavoriteItems/{itemId}/Delete` | 取消收藏 |
| POST | `/Users/{userId}/PlayedItems/{itemId}` | 标记已播放（递增 play_count） |
| POST | `/Users/{userId}/PlayedItems/{itemId}/Delete` | 标记未播放 |
| POST | `/Users/{userId}/Items/{itemId}/HideFromResume` | 从续播列表隐藏 |

### 播放会话 / Sessions

| Method | Endpoint | 说明 |
|--------|----------|------|
| GET | `/Sessions` | 活跃会话列表（自动清理超时） |
| POST | `/Sessions/Playing` | 上报播放开始 |
| POST | `/Sessions/Playing/Progress` | 上报播放进度 |
| POST | `/Sessions/Playing/Stopped` | 上报播放停止 |
| POST | `/Sessions/Capabilities` | 上报客户端能力 |
| POST | `/Sessions/Capabilities/Full` | 上报完整客户端能力 |

### 图片 / Images

| Method | Endpoint | 说明 |
|--------|----------|------|
| GET | `/Items/{itemId}/Images` | 图片列表 |
| GET | `/Items/{itemId}/Images/{type}` | 获取图片（支持 ETag/304） |
| POST | `/Items/{itemId}/Images/{type}` | 上传图片（支持 URL JSON） |
| POST | `/Items/{itemId}/Images/{type}/Delete` | 删除图片 |
| GET | `/Items/{itemId}/RemoteImages` | 远程图片搜索（TMDb） |
| POST | `/Items/{itemId}/RemoteImages/Download` | 下载远程图片 |

### 元数据 / Metadata

| Method | Endpoint | 说明 |
|--------|----------|------|
| POST | `/Items/{itemId}` | 编辑元数据（标题/概述/年份/类型/标签等） |
| GET | `/Items/{itemId}/ExternalIdInfos` | 外部 ID 信息 |
| POST | `/Items/RemoteSearch/{type}` | 远程元数据搜索（TMDb） |
| POST | `/Items/RemoteSearch/Apply/{itemId}` | 应用远程搜索结果 |
| POST | `/items/metadata/reset` | 重置元数据（清除标识） |
| POST | `/Items/{itemId}/Refresh` | 刷新媒体项 |
| GET | `/Items/{itemId}/DeleteInfo` | 删除预览（显示将删除的文件） |
| POST | `/Items/Delete` | 删除媒体项 |

### 其他 / Miscellaneous

| Method | Endpoint | 说明 |
|--------|----------|------|
| GET/POST | `/DisplayPreferences/{id}` | 视图偏好存取 |
| POST | `/Users/{userId}/Items/{itemId}/Rating` | 评分（喜欢/不喜欢/数值） |
| DELETE | `/Users/{userId}/Items/{itemId}/Rating` | 删除评分 |
| GET | `/Videos/{itemId}/AdditionalParts` | 视频附加部件 |
| GET | `/LiveTv/Channels` | 直播电视频道 |

### 合集与播放列表 / Collections & Playlists

| Method | Endpoint | 说明 |
|--------|----------|------|
| POST | `/Collections` | 创建合集（name + ids） |
| POST | `/Collections/{id}/Items` | 添加项目到合集 |
| DELETE | `/Collections/{id}/Items` | 从合集移除项目 |
| POST | `/Playlists` | 创建播放列表 |
| GET | `/Playlists/{id}` | 获取播放列表信息 |
| POST | `/Playlists/{id}` | 更新播放列表（名称/项目顺序） |
| GET | `/Playlists/{id}/Items` | 获取播放列表项目（分页） |
| POST | `/Playlists/{id}/Items` | 添加项目到播放列表 |
| DELETE | `/Playlists/{id}/Items` | 从播放列表移除项目 |

合集/播放列表通过 `linked_children` 表关联项目，支持 `ListItemIds` 反查包含指定项目的所有合集/播放列表。

### 媒体扫描 / Library Scanning

- 目录遍历与媒体分类（Movie/Series/Season/Episode）
- `.nfo` 元数据解析（标题/概述/年份/类型/标签/演员/工作室）
- 同目录封面图识别（poster/fanart/backdrop/thumb）
- 外挂字幕识别（srt/ass/ssa/vtt/sub）
- ffprobe 媒体探测（编码/分辨率/码率/时长/声道）

### 尚未实现 / Not Yet

- 转码 / HLS 串流
- 直播电视 / DVR
- 完整缺失集检测（需 TVDb/TMDb 集数据）
- TVDb / IMDb / MusicBrainz 提供者
- 插件系统 / SyncPlay / Quick Connect

## 构建 / Building

需要 Rust 1.85+ (edition 2024)。

```bash
cargo build --release
```

## License

GPL-2.0-only, matching Jellyfin's open source license.
