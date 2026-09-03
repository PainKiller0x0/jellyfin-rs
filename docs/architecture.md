# jellyfin-rs 架构设计

本文档记录当前实现的架构边界和关键数据流。它以代码为准，不描述尚未实现的完整 Jellyfin 能力。

## 1. 目标与边界

`jellyfin-rs` 是一个面向家庭媒体库的 Jellyfin/Emby API 兼容服务，优先保证：

- 本地媒体和 STRM 目标的直播放流；
- 中文媒体库的扫描、命名、封面和元数据；
- 常用 Jellyfin/Emby 客户端的浏览、播放、进度和管理接口；
- 在 PostgreSQL 和有限 VPS/NAS 资源上的可控并发。

通用转码、HLS 播放、完整插件系统和直播电视不属于当前核心实现；下载接口保留一个面向远程 STRM 的 HLS 兼容桥接。

## 2. 运行时结构

```text
HTTP request
    |
    v
jellyfin::routes              路由与 Axum Adapter
    |
    +--> endpoint modules      auth / items / playback / system / user_extras
    |         |
    |         +--> item_queries / common
    |         +--> playback::streaming
    |         +--> library providers
    |
    +--> AppState               DB、HTTP client、配置、队列、播放会话
              |
              +--> PostgreSQL entities and storage
              +--> library scanner / watcher / reconcile
              +--> TMDb / Douban / optional LLM metadata adapters
```

进程入口是 `src/main.rs`。它负责连接数据库、执行迁移、组装 `AppState`、启动计划任务和文件监听，然后把 `jellyfin::routes::api_routes()` 挂载到 Axum 应用。

## 3. Module 职责

| Module | 责任 | 主要 Interface / Implementation |
| --- | --- | --- |
| `jellyfin::routes` | 维护兼容 API 的路径到处理器映射 | `api_routes()`；处理器是 HTTP Adapter |
| `jellyfin::common` | 跨接口的响应、错误和 JSON 清理 | `internal_error`、通用响应构造 |
| `jellyfin::item_queries` | 媒体项 SQL、行解码、查询结果补充 | `media_item_select_sql`、`decode_media_items`、`visible_media_item_sql` |
| `jellyfin::items` | 浏览、详情、发现、剧集、推荐和元数据管理 | 各端点的请求解析与领域编排 |
| `jellyfin::playback` | 播放进度、收藏、播放状态和会话 | 用户数据处理与播放相关 API |
| `playback::streaming` | 解析播放目标并返回本地/远程媒体 | Range 响应、STRM 重定向/代理、下载专用 HLS 桥接、字幕响应 |
| `library::scanner` | 遍历媒体库并识别媒体项 | 扫描、入库、探测和旁车文件处理 |
| `library::storage` | 将扫描、探测和元数据写入数据库 | 媒体项、流、关系和外部流的持久化 |
| `library::naming` / `classify` | 文件名解析、媒体类型和剧集编号识别 | 纯本地解析实现 |
| `library::reconcile` | 合并同一作品的重复来源和版本 | 来源优先级、系列/电影归并 |
| `library::tmdb_metadata` / `douban_metadata` | 外部元数据 Adapter | 配置启用后搜索、获取、写回元数据 |
| `library::watcher` | 文件变化监听和轮询兜底 | 将变化映射为局部或全库扫描 |
| `db` / `entities` | 数据库连接、迁移和 SeaORM 模型 | PostgreSQL schema 与查询基础设施 |

## 4. 关键数据流

### 4.1 媒体入库

```text
library path
    -> scanner
    -> classify + naming
    -> probe / image / NFO / sidecar discovery
    -> storage.upsert_media_item
    -> metadata provider queue (if enabled)
    -> reconcile duplicate providers/versions
```

文件监听器会把变化路径映射到合适的扫描范围；启动扫描和手动刷新走同一套入库实现。媒体流、旁车字幕和旁车音频分别持久化，但都关联到同一个媒体项。

### 4.2 播放

```text
client playback request
    -> route Adapter
    -> item/source lookup
    -> PlaybackTarget
    -> local Range response or remote STRM redirect/proxy
```

`streaming.rs` 中的多个公开函数是为兼容不同 Jellyfin 路径和 HTTP 方法保留的薄 Adapter；实际媒体解析和响应逻辑集中在内部实现中。因此这些入口不应为了减少函数数量而强行合并。

外部 `.ass`/`.ssa` 字幕按原始格式返回，嵌入式字幕仍按缓存和 VTT/track-events 流程处理。外部旁车字幕只在同目录并且文件名与媒体主文件匹配时入库：支持精确主文件名、主文件名加后缀和同季同集匹配。

### 4.3 下载

```text
client download request
    -> /Items/{id}/Download
    -> local file Range response
       or remote STRM target probe
          -> direct MP4 redirect
          -> HLS-only ffmpeg remux to fragmented MP4
```

下载路径与播放路径保持不同策略：远程媒体源的 `DirectStreamUrl` 继续指向原始直链供在线播放，`Path`/`DownloadUrl` 指向配置的 Jellyfin 公网地址下的 `/Items/{id}/Download/{encoded filename}` 供支持下载的客户端使用；旧的 `/Download` 与 `/Download.{container}` 入口仍保留用于客户端兼容。生产环境的下载地址附带仅绑定当前媒体项且有有效期的签名参数，避免为兼容下载客户端而公开整个下载接口。未配置公网地址或签名密钥时回退为相对路径。远程 STRM 原名到实际下载文件名的规则集中在 `library::naming::download_filename_from_path`，避免模型响应和下载响应产生不同文件名。远程目标只有在下载请求中才做媒体类型探测。已经返回可分段下载的 MP4 时继续 307 直跳，不经过 jellyfin-rs；检测到 HLS 播放列表时，才由 jellyfin-rs 临时转封装为 MP4，并在媒体元数据提供已知大小时补充 `Content-Length`，不把 HLS 播放列表大小误当作视频大小。播放请求不触发该桥接，因此不会改变夸克直链播放的流量路径。

### 4.4 元数据

TMDb、豆瓣和 LLM 是独立的外部 Adapter。调用前先检查对应的设置开关和凭据；未启用或未配置时跳过，避免无效网络请求。结果通过 `library::storage` 写入媒体项、季、集、人物和图片关系。

## 5. 公共 Seam 与不变量

### 5.1 公开媒体项谓词

`jellyfin::item_queries::visible_media_item_sql(alias)` 是所有面向普通客户端的媒体查询共享的 SQL Seam。它要求：

1. 当前项 `is_public = 1`；
2. 顶层项、直接位于 library 下的项，或公开父级下的项才可见；
3. 管理员专用查询可以显式选择绕过该谓词。

这样可以避免搜索、详情、Next Up、推荐、过滤器、人物和播放进度接口各自维护一份容易漂移的可见性规则。

### 5.2 媒体项查询

`media_item_select_sql` 和相关解码逻辑是媒体项列表的核心公共 Seam。端点模块负责参数和业务条件，`item_queries` 负责公共字段选择、数据库行解码以及通用补充信息。

### 5.3 旁车文件

字幕和音频共用同目录文件枚举实现，但保留各自的筛选、探测和持久化语义：

- 音频候选按路径排序后探测，并整体替换外部音频流；
- 字幕按扫描顺序写入外部流索引，并先清除旧的外部字幕流；
- 语言别名只共享重叠的通用映射，字幕未知语言仍保留原有短 token 回退行为。

## 6. 依赖方向与扩展规则

- 新增 API：先在 `jellyfin::routes` 注册，再在对应 endpoint Module 中实现 Adapter；公共媒体 SQL 优先复用 `item_queries`。
- 新增媒体识别：放在 `library::classify` 或 `library::naming`，不要把文件名规则复制到 endpoint Module。
- 新增持久化字段/流：通过 `entities`、schema 迁移和 `library::storage` 完成，不在路由中直接散落写库逻辑。
- 新增元数据来源：实现独立 provider Adapter，复用 storage 写回；不要把 TMDb/Douban 的网络协议混进公共查询 Module。
- 新增播放格式：先扩展 `PlaybackTarget` 或内部播放实现，只有在客户端路径确实不同的时候才新增公开 Adapter。
- 递归媒体树查询暂不抽成一个万能函数：向下遍历、向上解析、删除后代和额外内容查询的根节点与可见性语义不同，贸然合并会降低 Module Depth。

## 7. 本次重构记录

本次审查完成了两类低风险公共实现提取：

1. 将 10 个 endpoint Module 中重复的公开媒体项 SQL 谓词集中到 `jellyfin::item_queries`，减少重复实现并统一未来修改入口。
2. 将旁车字幕/音频的同目录文件枚举和通用字幕/音频语言别名映射集中到 `library::subtitles` 的私有实现，保持音频排序、字幕索引和未知语言行为不变。
3. 将远程 STRM 下载文件名规范集中到 `library::naming::download_filename_from_path`，并让媒体源 JSON 与实际下载响应共用同一规则；同时删除不再使用的旧下载 URL/token 包装层。
4. 保持不同下载路径的公开 Adapter 为薄入口，共享 `download_item_response`；保持 `remote_stream_client`、文件名响应头和 HLS 下载响应的公共实现，避免兼容路径各自维护一套行为。

以下相似代码本次保留：

- 播放路由薄 Adapter：它们承担客户端路径和 HTTP 方法兼容，删除会降低接口兼容性；
- 递归媒体树 SQL：查询方向和根节点不同；
- TMDb 与豆瓣 provider：协议和解析差异足以形成独立 Adapter。
- 媒体源 JSON 的主项与子项构造：两者虽然字段相似，但来源标识、媒体流和版本语义不同；合并成一个超长参数函数会降低可读性和 Module Depth。

## 8. 验证

```bash
cargo fmt --check
cargo test
cargo build --release
pnpm --dir admin build
```

公共可见性变更应重点回归普通用户的列表、详情、推荐、Next Up、过滤器、人物和播放进度接口；旁车变更应重点回归外部音频、外部 `ass/ssa` 字幕、嵌入字幕索引以及同集不同版本的匹配。
