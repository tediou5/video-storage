# Video Storage API 文档

## 概述

Video Storage 服务提供两组 API：
- **外部 API** (端口 32145): 需要 claim 令牌认证，用于视频文件访问
- **内部 API** (端口 32146): 无需认证，用于管理操作

两个 API 都监听在 `0.0.0.0` 上，可以通过配置文件或命令行参数指定端口。

## 内部 API 接口

内部 API 默认监听在 `0.0.0.0:32146`

### 1. 创建认证令牌 (Create Claim)

创建用于访问视频的认证令牌。

**接口地址**: `POST /claims`
**认证要求**: 无

#### 请求参数 (JSON Body)

| 参数 | 类型 | 必填 | 说明 | 限制 |
|-----|------|-----|------|------|
| asset_id | string\|array | 是 | 资源ID，对应视频的 job_id；字符串=v1凭证(单资产)，数组=v2凭证(多资产) | 不能为空 |
| exp_unix | u32 | 是 | 令牌过期时间（Unix时间戳） | 必须大于 nbf_unix |
| nbf_unix | u32 | 否 | 令牌生效时间（Unix时间戳） | 默认为当前时间 |
| window_len_sec | u16 | 否 | 时间窗口长度（秒） | 0-65535，默认为0（无限制） |
| max_kbps | u16 | 否 | 最大传输速率（kbps） | 0-65535，默认为0（无限制） |
| max_concurrency | u16 | 否 | 最大并发连接数 | 0-65535，默认为0（无限制） |
| allowed_widths | Vec<u16> | 否 | 允许访问的视频宽度列表 | 数组，默认为空（允许所有宽度） |

#### 响应格式

成功响应 (200 OK):
```json
{
  "token": "加密的令牌字符串"
}
```

错误响应 (400 Bad Request):
- asset_id 为空
- exp_unix <= nbf_unix

错误响应 (500 Internal Server Error):
- 令牌签名失败

#### CURL 示例

```bash
# 创建一个简单的v1令牌（单资产，所有限制参数使用默认值）
curl -X POST http://localhost:32146/claims \
  -H "Content-Type: application/json" \
  -d '{
    "asset_id": "video123",
    "exp_unix": '$(($(date +%s) + 3600))'
  }'

# 创建一个带部分限制的v1令牌
curl -X POST http://localhost:32146/claims \
  -H "Content-Type: application/json" \
  -d '{
    "asset_id": "video123",
    "exp_unix": '$(($(date +%s) + 3600))',
    "max_kbps": 5000,
    "allowed_widths": [1920, 1280]
  }'

# 创建一个v2令牌（多资产访问）
curl -X POST http://localhost:32146/claims \
  -H "Content-Type: application/json" \
  -d '{
    "asset_id": ["video1", "video2", "video3"],
    "exp_unix": '$(($(date +%s) + 3600))',
    "max_kbps": 8000,
    "max_concurrency": 5,
    "allowed_widths": [1920, 1280]
  }'

# 创建一个完整配置的v1令牌
curl -X POST http://localhost:32146/claims \
  -H "Content-Type: application/json" \
  -d '{
    "asset_id": "video123",
    "exp_unix": '$(($(date +%s) + 3600))',
    "nbf_unix": '$(date +%s)',
    "window_len_sec": 300,
    "max_kbps": 5000,
    "max_concurrency": 3,
    "allowed_widths": [1920, 1280, 854, 640]
  }'
```

### 2. 上传视频文件进行转换

上传 MP4 文件并触发 HLS 转换任务。

**接口地址**: `POST /upload`
**认证要求**: 无

#### 查询参数 (Query Parameters)

| 参数 | 类型 | 必填 | 说明 | 限制 |
|-----|------|-----|------|------|
| id | string | 是 | 任务ID | 不能包含 '/', '-', '.', ' ' |
| crf | u8 | 是 | 视频压缩质量参数 | 0-63，值越小质量越高 |
| dst_bucket | string | 否* | 目标桶（S3 模式下 HLS 产物会上传到该桶） | 不能为空、不能包含 `/`、不允许纯数字 |

> \* 当 `storage_backend = "s3"` 时，`dst_bucket` 为必填；当 `storage_backend = "local"` 时会被忽略。

#### 请求体

- Content-Type: application/octet-stream
- Body: MP4 视频文件的二进制数据

#### 响应格式

成功响应 (202 Accepted):
```json
{
  "job_id": "video123",
  "message": "Processing in background"
}
```

错误响应 (400 Bad Request):
```json
{
  "job_id": "video123",
  "message": "错误信息"
}
```

错误信息可能包括：
- "Invalid parameters: crf can only be set in the range 0-63"
- "already in-progress" (任务已在处理中)
- "Invalid filename, Cannot contain '/', '-', ' ' and '.'"
- "Failed to create upload job file"
- "Failed to write to upload job file"

#### CURL 示例

```bash
# 上传视频文件进行转换，CRF=23（适中质量）
curl -X POST "http://localhost:32146/upload?id=video123&crf=23" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @input.mp4

# S3 模式：需要指定 dst_bucket
curl -X POST "http://localhost:32146/upload?id=video123&crf=23&dst_bucket=my-bucket" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @input.mp4
```

#### 只转码h265(特殊用例)

```bash
# 上传视频文件进行转换，只转码h265
curl -g -X POST "http://localhost:32146/upload?id=video2&crf=38&codecs[0]=h265" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @input.mp4
```

### 3. 查询任务队列状态

获取当前待处理任务的统计信息。

**接口地址**: `GET /waitlist`
**认证要求**: 无

#### 请求参数

无

#### 响应格式

成功响应 (200 OK):
```json
{
  "pending_convert_jobs": 5,      // 待处理的转换任务数
  "pending_migrate_jobs": 1,      // 待处理的迁移任务数
  "pending_upload_jobs": 2,       // 待处理的上传任务数
  "total_pending_jobs": 8         // 总待处理任务数
}
```

#### CURL 示例

```bash
# 查询任务队列状态
curl http://localhost:32146/waitlist
```

### 4. 迁移视频文件到新桶 (Migrate)

将指定 `job_id` 对应的 HLS 产物从 `src_bucket` 迁移到 `dst_bucket`。

**接口地址**: `POST /migrate`
**认证要求**: 无（仅内部 API）
**后端要求**: 仅当 `storage_backend = "s3"` 时可用

> 注意：当前版本迁移完成后**不会删除源桶对象**（为安全起见暂时禁用删除）。

#### 请求参数 (JSON Body)

| 参数 | 类型 | 必填 | 说明 |
|-----|------|-----|------|
| job_id | string | 是 | 任务ID（同 `/upload?id=...`） |
| src_bucket | string | 是 | 源桶 |
| dst_bucket | string | 是 | 目标桶 |
| widths | Vec<u16> | 否 | 需要迁移的清晰度宽度列表；不传则使用服务内置默认分辨率列表 |

Bucket 限制：
- 不能为空
- 不能包含 `/`
- 不允许为纯数字（例如 `123`），以避免与清晰度路径（如 `720/...`）产生歧义

#### 响应格式

成功响应 (202 Accepted):
```json
{
  "job_id": "video123",
  "message": "Processing in background"
}
```

说明：
- `/migrate` 现在是异步任务接口，成功入队后立即返回。
- 迁移成功时会发送通用 webhook；失败仅记录日志，不回调上游。

错误响应：

- 400 Bad Request:
```json
{ "job_id": "video123", "message": "错误信息" }
```

- 500 Internal Server Error:
```json
{ "job_id": "video123", "message": "错误信息" }
```

#### CURL 示例

```bash
curl -X POST http://localhost:32146/migrate \
  -H "Content-Type: application/json" \
  -d '{
    "job_id": "video123",
    "src_bucket": "old-bucket",
    "dst_bucket": "new-bucket",
    "widths": [480]
  }'
```

## 外部 API 接口

外部 API 默认监听在 `0.0.0.0:32145`

### 5. 获取视频文件

获取转换后的视频文件（HLS 格式）。

**接口地址**: `GET /videos/{bucket}/{key}`
**认证要求**: 需要有效的 claim 令牌

> 说明：
> - 在 `storage_backend = "s3"` 时，必须显式带上 `bucket`，服务会按 bucket 读取对象存储。
> - 兼容模式：如果配置了 `s3_bucket`（或启动参数 `--s3-bucket` / 环境变量 `S3_BUCKET`），则也允许 legacy 路径 `GET /videos/{key}` 或 `GET /videos/{width}/{key}`，此时会使用默认桶读取。
> - 在 `storage_backend = "local"` 时，`bucket` 会被忽略（但建议统一带上，便于未来切换到 S3）。

#### 路径参数

| 参数 | 类型 | 必填 | 说明 |
|-----|------|-----|------|
| bucket | string | 是 | 桶名（由上层服务决定） |
| key | string | 是 | 桶内对象 key。典型示例：`{job_id}.m3u8`、`480/{job_id}.m3u8`、`480/{job_id}-001.m4s`、`480/{job_id}-init.mp4` |

#### 请求头

| 头部 | 必填 | 说明 |
|------|-----|------|
| Authorization | 是 | `Bearer <token>` |
| Range | 否 | 支持范围请求，格式: `bytes=start-end` |

#### 响应格式

成功响应 (200 OK 或 206 Partial Content):
- Content-Type: 根据文件类型自动判断
  - .m3u8 文件: application/vnd.apple.mpegurl
  - .m4s 文件: video/iso.segment
  - .mp4 文件: video/mp4
- Accept-Ranges: bytes
- Cache-Control: public,max-age=3600
- Content-Length: 文件大小或范围大小
- Content-Range: bytes start-end/total (仅在范围请求时)
- Body: 文件内容

错误响应:
- 401 Unauthorized: 令牌无效或过期
- 403 Forbidden: 资源访问被拒绝
- 404 Not Found: 文件不存在
- 429 Too Many Requests: 超过速率限制

#### CURL 示例

```bash
# 获取 HLS 播放列表
curl -H "Authorization: Bearer your_token_here" \
  http://localhost:32145/videos/my-bucket/video123.m3u8

# 获取视频片段
curl -H "Authorization: Bearer your_token_here" \
  http://localhost:32145/videos/my-bucket/480/video123-001.m4s

# 使用范围请求
curl -H "Authorization: Bearer your_token_here" \
  -H "Range: bytes=0-1048575" \
  http://localhost:32145/videos/my-bucket/480/video123-001.m4s
```

## 速率限制

### 令牌级别限制

每个 claim 令牌包含以下限制参数（均为可选）：
- `max_kbps`: 最大传输速率（千比特/秒），0 表示无限制
- `max_concurrency`: 最大并发连接数，0 表示无限制
- `window_len_sec`: 时间窗口长度，0 表示无限制

### 全局限制

服务器配置的全局速率限制：
- `token_rate`: 全局令牌桶速率（默认为 0，表示无限制）

## 错误码说明

### HTTP 状态码

| 状态码 | 说明 |
|-------|------|
| 200 | 请求成功 |
| 202 | 已接受，任务在后台处理 |
| 206 | 部分内容（范围请求） |
| 400 | 请求参数错误 |
| 401 | 未授权，令牌无效 |
| 403 | 禁止访问，权限不足 |
| 404 | 资源不存在 |
| 429 | 请求过多，超过速率限制 |
| 500 | 服务器内部错误 |

### Claim 错误码

在使用 claim 令牌时，可能遇到以下错误：

| 错误码 | 说明 |
|-------|------|
| invalid_token | 令牌格式错误或签名无效 |
| token_expired | 令牌已过期 |
| token_not_yet_valid | 令牌尚未生效 |
| asset_mismatch | 访问的资源与令牌不匹配 |
| time_window_deny | 访问时间超出允许窗口 |
| key_not_found | 找不到对应的密钥 |

## 完整工作流程示例

### 1. 上传视频并转换

```bash
# 上传 MP4 视频进行转换
curl -X POST "http://localhost:32146/upload?id=myvideo&crf=23" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @video.mp4

# 检查任务队列
curl http://localhost:32146/waitlist
```

### 2. 创建访问令牌

```bash
# 创建一个最简单的v1令牌（单资产，无任何限制）
curl -X POST http://localhost:32146/claims \
  -H "Content-Type: application/json" \
  -d '{
    "asset_id": "myvideo",
    "exp_unix": '$(($(date +%s) + 86400))'
  }'

# 创建一个带速率限制的v1令牌
curl -X POST http://localhost:32146/claims \
  -H "Content-Type: application/json" \
  -d '{
    "asset_id": "myvideo",
    "exp_unix": '$(($(date +%s) + 86400))',
    "max_kbps": 10000,
    "max_concurrency": 5
  }'

# 创建一个v2令牌（多资产访问）
curl -X POST http://localhost:32146/claims \
  -H "Content-Type: application/json" \
  -d '{
    "asset_id": ["myvideo", "episode1", "episode2"],
    "exp_unix": '$(($(date +%s) + 86400))',
    "max_kbps": 15000,
    "max_concurrency": 3,
    "allowed_widths": [1920, 1280]
  }'

# 创建一个完整配置的v1令牌
curl -X POST http://localhost:32146/claims \
  -H "Content-Type: application/json" \
  -d '{
    "asset_id": "myvideo",
    "exp_unix": '$(($(date +%s) + 86400))',
    "window_len_sec": 3600,
    "max_kbps": 10000,
    "max_concurrency": 5,
    "allowed_widths": [1920, 1280, 854]
  }'
```

### 3. 使用令牌访问视频

```bash
# 假设返回的令牌为 TOKEN
TOKEN="your_token_here"
BUCKET="my-bucket"

# 获取 HLS 主播放列表
curl -H "Authorization: Bearer $TOKEN" \
  http://localhost:32145/videos/$BUCKET/myvideo.m3u8 > playlist.m3u8

# 获取某个清晰度的播放列表（例如 480p）
curl -H "Authorization: Bearer $TOKEN" \
  http://localhost:32145/videos/$BUCKET/480/myvideo.m3u8 > playlist-480.m3u8

# 获取 init segment / media segment
curl -H "Authorization: Bearer $TOKEN" \
  http://localhost:32145/videos/$BUCKET/480/myvideo-init.mp4 > init.mp4
curl -H "Authorization: Bearer $TOKEN" \
  http://localhost:32145/videos/$BUCKET/480/myvideo-001.m4s > segment1.m4s
```

#### 获取h265视频片段

```bash
# 假设返回的令牌为 TOKEN
TOKEN="your_token_here"
BUCKET="my-bucket"

# 获取 HLS 主播放列表
curl -H "Authorization: Bearer $TOKEN" \
  http://localhost:32145/videos/$BUCKET/myvideo-h265.m3u8 > h265-playlist.m3u8

# 获取h265视频片段（例如 480p）
curl -H "Authorization: Bearer $TOKEN" \
  http://localhost:32145/videos/$BUCKET/480/myvideo-h265-001.m4s > h265-segment1.m4s
```

## 配置说明

### 服务端口配置

```toml
# config.toml
listen_on_port = 32145  # 外部 API 端口
internal_port = 32146    # 内部 API 端口
```

通过命令行参数：
```bash
video-storage --listen-on-port 8080 --internal-port 8081
```

### 存储配置

支持本地存储和 S3 兼容存储：

```toml
# 本地存储
storage_backend = "local"
workspace = "./data"

# S3 存储
storage_backend = "s3"
s3_endpoint = "http://localhost:9000"  # MinIO 或自定义 S3
s3_region = "us-east-1"
s3_bucket = "my-bucket"               # 可选：用于 legacy /videos/<key> 读取的默认桶
s3_access_key_id = "minioadmin"
s3_secret_access_key = "minioadmin"
```

当使用 `storage_backend = "s3"` 时，bucket 由上层服务在请求/任务参数中指定：
- 上传/转码：`POST /upload?...&dst_bucket=<bucket>`
- 播放读取：`GET /videos/<bucket>/<key>`
- 迁移对象：`POST /migrate {src_bucket, dst_bucket, job_id, ...}`，异步返回 `202`
- 注意：`dst_bucket` 不允许为纯数字（例如 `123`），以避免与清晰度路径（如 `720/...`）产生歧义。

### 认证密钥配置

```toml
# 可选：配置固定的认证密钥
# 如不配置，服务启动时会自动生成随机密钥
[claim_keys]
1 = "IaNHoHtWetGMPkHj6Iy8MZe5L3KlH8F6j6nRvJpYQYU="  # openssl rand -base64 32
2 = "uBhfVeH0b7KQKfwOJqhwzLXKBpg7xLPBe5HjCksDDWg="
```

## 注意事项

1. **文件名限制**:
   - job_id 不能包含 '/', '-', '.', ' ' 等特殊字符
   - 对象文件名不能包含 '/', ' '

2. **CRF 参数**: 范围 0-63，推荐值：
   - 18-23: 高质量，文件较大
   - 23-28: 标准质量，平衡质量和文件大小
   - 28-35: 低质量，文件较小

3. **令牌版本选择**:
   - **v1令牌(单资产)**: asset_id 为字符串格式，适用于单个视频访问
   - **v2令牌(多资产)**: asset_id 为数组格式，适用于批量访问、播放列表等场景
   - 系统根据 asset_id 参数类型自动选择版本

4. **令牌参数说明**:
   - 所有限制参数（window_len_sec, max_kbps, max_concurrency, allowed_widths）均为可选
   - 未指定或设为 0 表示无限制
   - allowed_widths 为空数组表示允许所有分辨率
   - v2令牌建议资产数量控制在100以内以保证最佳性能

5. **速率限制**:
   - 合理配置速率限制参数，避免带宽滥用
   - max_kbps = 0 表示不限速
   - max_concurrency = 0 表示不限制并发数

## 监控和运维

### 健康检查

```bash
# 检查服务状态
curl http://localhost:32146/waitlist
```

### 日志配置

服务使用 `tracing` 进行日志记录，支持以下日志级别：
- TRACE
- DEBUG
- INFO
- WARN
- ERROR

设置环境变量控制日志级别：
```bash
RUST_LOG=info video-storage
```

### Webhook 通知

配置 webhook URL 后，任务完成时会发送通知：

```json
{
  "job_id": "video123",
  "job_type": "convert",
  "status": "completed",
  "timestamp": "2025-01-09T12:34:56Z"
}
```

## API 版本历史

### v1.0.0 (当前版本)
- 支持 HLS 视频流
- Claim 令牌认证 (v1单资产 + v2多资产)
- 高效的多资产过滤器 (BinaryFuse16)
- 速率限制功能
- S3 存储支持
- 灵活的令牌限制参数（全部可选）
