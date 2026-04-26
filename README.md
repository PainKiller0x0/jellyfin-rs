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
- **SQLite 默认** — 通过 `JELLYFIN_RS_DATABASE_URL` 可切换 PostgreSQL

## 快速开始 / Quick Start

```bash
# 设置环境变量
export JELLYFIN_RS_MEDIA_DIRS="/data/movies;/data/tvshows"
export JELLYFIN_RS_USER="admin"
export JELLYFIN_RS_PASSWORD="your-password"

# 可选：TMDb API Key 用于元数据
export JELLYFIN_RS_TMDB_API_KEY="your-tmdb-key"

cargo run --release
```

服务默认监听 `http://127.0.0.1:8096`。使用管理员账号连接任意 Jellyfin 客户端。

## 配置 / Configuration

| 环境变量 | 默认值 | 说明 |
|----------|--------|------|
| `JELLYFIN_RS_HOST` | `127.0.0.1` | 监听地址 |
| `JELLYFIN_RS_PORT` | `8096` | 监听端口 |
| `JELLYFIN_RS_DATABASE_URL` | `sqlite://jellyfin-rs.db` | 数据库连接 |
| `JELLYFIN_RS_MEDIA_DIRS` | (无) | 媒体目录，分号分隔 |
| `JELLYFIN_RS_SCAN_ON_STARTUP` | `true` | 启动时扫描媒体库 |
| `JELLYFIN_RS_USER` | `tsukimi` | 默认管理员用户名 |
| `JELLYFIN_RS_PASSWORD` | `tsukimi` | 默认管理员密码 |
| `JELLYFIN_RS_TMDB_API_KEY` | (无) | TMDb API Key |
| `JELLYFIN_RS_FFPROBE_PATH` | (系统 PATH) | ffprobe 路径 |
| `JELLYFIN_RS_SESSION_TIMEOUT_SECONDS` | `120` | 会话超时秒数 |

## API 覆盖 / API Coverage

### 已实现 (75+ 端点)

**系统:** 服务器信息、活动日志、计划任务、关机重启
**媒体库:** 虚拟文件夹增删改查、刷新扫描
**用户:** 登录认证、增删改查、密码修改、头像
**媒体项:** 浏览、搜索、过滤、最新、续播、相似、计数
**剧集:** 季/集列表、下一集
**播放:** 播放信息、直出串流、进度上报
**图片:** 上传/获取(ETag/304)/删除/远程搜索(TMDb)
**元数据:** 编辑、远程搜索/应用、重置、NFO 解析
**会话:** 活跃会话、客户端能力、超时清理
**过滤器:** 类型、年代、标签、人物、工作室、容器、编码
**偏好:** 视图偏好存取
**评分:** 喜欢/不喜欢、数值评分

### 尚未实现

- 转码 / HLS 串流
- 直播电视 / DVR
- 插件系统
- SyncPlay
- 合集 / 播放列表
- TVDb / IMDb / MusicBrainz 提供者

## 构建 / Building

需要 Rust 1.85+ (edition 2024)。

```bash
cargo build --release
```

## License

MIT
