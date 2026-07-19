# Emby OpenAPI Contract Tests

这些测试只读取 `docs/emby-openapi.json`，不会依赖项目源码。默认目标是本机 Emby/Jellyfin 兼容服务：

```bash
uv run --project tests/openapi_contract pytest tests/openapi_contract
```

默认配置：

- `EMBY_BASE_URL=http://127.0.0.1:8096`
- `EMBY_USERNAME=admin`
- `EMBY_PASSWORD=123456`
- `EMBY_OPENAPI=docs/emby-openapi.json`
- `EMBY_CONTRACT_REPORT=tests/openapi_contract/contract-report.jsonl`

测试会先调用 `/Users/AuthenticateByName` 认证，再从用户可见资源中发现 `UserId`、item、folder、media source、session 等上下文。每个 OpenAPI operation 会生成独立 pytest 用例，每个可选参数也会生成独立用例，不会把多个 API 混在一个断言里。

默认只跑用户/媒体浏览相关接口，并跳过明显的管理员接口。带副作用的用户接口默认不跑；确实要跑时：

```bash
EMBY_CONTRACT_INCLUDE_MUTATING=true uv run --project tests/openapi_contract pytest tests/openapi_contract
```

返回校验包含：

- HTTP 状态必须是文档声明的成功状态。
- JSON 响应必须符合 OpenAPI schema。
- 默认把 OpenAPI schema 未声明的对象字段视为失败；如果只想检查字段类型和状态码，可设置 `EMBY_CONTRACT_STRICT_ADDITIONAL_PROPERTIES=false`。
- OpenAPI 未声明响应 body 时，服务端实际返回 body 会被判定失败。
- 每条用例都会写入 JSONL 报告，包含实际状态、期望状态、请求参数、schema 错误和返回片段。

如果希望忽略文档未声明字段：

```bash
EMBY_CONTRACT_STRICT_ADDITIONAL_PROPERTIES=false uv run --project tests/openapi_contract pytest tests/openapi_contract
```
