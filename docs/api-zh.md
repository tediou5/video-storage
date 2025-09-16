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
| asset_id | string | 是 | 资源ID，对应视频的 job_id | 不能为空 |
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
# 创建一个简单的令牌（所有限制参数使用默认值）
curl -X POST http://localhost:32146/claims \
  -H "Content-Type: application/json" \
  -d '{
    "asset_id": "video123",
    "exp_unix": '$(($(date +%s) + 3600))'
  }'

# 创建一个带部分限制的令牌
curl -X POST http://localhost:32146/claims \
  -H "Content-Type: application/json" \
  -d '{
    "asset_id": "video123",
    "exp_unix": '$(($(date +%s) + 3600))',
    "max_kbps": 5000,
    "allowed_widths": [1920, 1280]
  }'

# 创建一个完整配置的令牌
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

#### 请求体

- Content-Type: video/mp4 或 application/octet-stream
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
  -H "Content-Type: video/mp4" \
  --data-binary @input.mp4
```

### 3. 上传视频对象文件

上传与视频相关的其他文件（如字幕、缩略图等）。

**接口地址**: `POST /upload-objects`
**认证要求**: 无

#### 查询参数 (Query Parameters)

| 参数 | 类型 | 必填 | 说明 | 限制 |
|-----|------|-----|------|------|
| id | string | 是 | 视频任务ID | 不能包含 '/', '-', '.', ' ' |
| name | string | 是 | 对象文件名 | 不能包含 '/', ' ' |

#### 请求体

- Content-Type: application/octet-stream
- Body: 文件的二进制数据

#### 响应格式

成功响应 (200 OK):
```json
{
  "job_id": "video123",
  "message": "Upload object done"
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
- "Invalid id, Cannot contain '/', '-', ' ' and '.'"
- "Invalid object name, Cannot contain '/' and ' '"
- "Failed to create upload job object file"
- "Failed to write to upload job object file"

#### CURL 示例

```bash
# 上传字幕文件
curl -X POST "http://localhost:32146/upload-objects?id=video123&name=subtitle_en.vtt" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @subtitle.vtt

# 上传缩略图
curl -X POST "http://localhost:32146/upload-objects?id=video123&name=thumbnail.jpg" \
  -H "Content-Type: image/jpeg" \
  --data-binary @thumb.jpg
```

### 4. 获取视频对象文件

获取已上传的视频对象文件。

**接口地址**: `GET /objects/{job_id}/{filename}`
**认证要求**: 无

#### 路径参数

| 参数 | 类型 | 必填 | 说明 |
|-----|------|-----|------|
| job_id | string | 是 | 视频任务ID |
| filename | string | 是 | 对象文件名 |

#### 请求头 (可选)

| 头部 | 说明 |
|------|------|
| Range | 支持范围请求，格式: `bytes=start-end` |

#### 响应格式

成功响应 (200 OK 或 206 Partial Content):
- Content-Type: 根据文件扩展名自动判断
- Accept-Ranges: bytes
- Cache-Control: public,max-age=3600
- Content-Length: 文件大小或范围大小
- Content-Range: bytes start-end/total (仅在范围请求时)
- Body: 文件内容

错误响应 (404 Not Found):
- 文件不存在

#### CURL 示例

```bash
# 获取完整文件
curl -O http://localhost:32146/objects/video123/subtitle_en.vtt

# 范围请求（获取前1MB）
curl -H "Range: bytes=0-1048575" \
  http://localhost:32146/objects/video123/thumbnail.jpg
```

### 5. 查询任务队列状态

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
  "pending_upload_jobs": 2,       // 待处理的上传任务数
  "total_pending_jobs": 7         // 总待处理任务数
}
```

#### CURL 示例

```bash
# 查询任务队列状态
curl http://localhost:32146/waitlist
```

## 外部 API 接口

外部 API 默认监听在 `0.0.0.0:32145`

### 6. 获取视频文件

获取转换后的视频文件（HLS 格式）。

**接口地址**: `GET /videos/{filename}`
**认证要求**: 需要有效的 claim 令牌

#### 路径参数

| 参数 | 类型 | 必填 | 说明 |
|-----|------|-----|------|
| filename | string | 是 | 视频文件名，格式通常为 `{job_id}-{resolution}.m3u8` 或 `{job_id}-{resolution}-{segment}.ts` |

#### 请求头

| 头部 | 必填 | 说明 |
|------|-----|------|
| X-Claim-Token | 是 | 认证令牌 |
| Range | 否 | 支持范围请求，格式: `bytes=start-end` |

#### 响应格式

成功响应 (200 OK 或 206 Partial Content):
- Content-Type: 根据文件类型自动判断
  - .m3u8 文件: application/vnd.apple.mpegurl
  - .ts 文件: video/mp2t
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
curl -H "X-Claim-Token: your_token_here" \
  http://localhost:32145/videos/video123-1080.m3u8

# 获取视频片段
curl -H "X-Claim-Token: your_token_here" \
  http://localhost:32145/videos/video123-1080-00001.ts

# 使用范围请求
curl -H "X-Claim-Token: your_token_here" \
  -H "Range: bytes=0-1048575" \
  http://localhost:32145/videos/video123-1080-00001.ts
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
  -H "Content-Type: video/mp4" \
  --data-binary @video.mp4

# 检查任务队列
curl http://localhost:32146/waitlist

# 上传相关文件（可选）
curl -X POST "http://localhost:32146/upload-objects?id=myvideo&name=poster.jpg" \
  --data-binary @poster.jpg
```

### 2. 创建访问令牌

```bash
# 创建一个最简单的令牌（无任何限制）
curl -X POST http://localhost:32146/claims \
  -H "Content-Type: application/json" \
  -d '{
    "asset_id": "myvideo",
    "exp_unix": '$(($(date +%s) + 86400))'
  }'

# 创建一个带速率限制的令牌
curl -X POST http://localhost:32146/claims \
  -H "Content-Type: application/json" \
  -d '{
    "asset_id": "myvideo",
    "exp_unix": '$(($(date +%s) + 86400))',
    "max_kbps": 10000,
    "max_concurrency": 5
  }'

# 创建一个完整配置的令牌
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

# 获取 HLS 主播放列表
curl -H "X-Claim-Token: $TOKEN" \
  http://localhost:32145/videos/myvideo-1080.m3u8 > playlist.m3u8

# 获取视频片段
curl -H "X-Claim-Token: $TOKEN" \
  http://localhost:32145/videos/myvideo-1080-00001.ts > segment1.ts
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
s3_bucket = "my-video-bucket"
s3_endpoint = "http://localhost:9000"  # MinIO 或自定义 S3
s3_region = "us-east-1"
s3_access_key_id = "minioadmin"
s3_secret_access_key = "minioadmin"
```

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

3. **令牌参数说明**:
   - 所有限制参数（window_len_sec, max_kbps, max_concurrency, allowed_widths）均为可选
   - 未指定或设为 0 表示无限制
   - allowed_widths 为空数组表示允许所有分辨率

4. **令牌安全**:
   - claim 令牌应该妥善保管
   - 不要在客户端代码中硬编码令牌
   - 建议使用 HTTPS 传输令牌

5. **速率限制**:
   - 合理配置速率限制参数，避免带宽滥用
   - max_kbps = 0 表示不限速
   - max_concurrency = 0 表示不限制并发数

6. **缓存策略**:
   - 视频文件默认设置 1 小时缓存
   - 建议配合 CDN 使用以提高性能

7. **存储后端**:
   - 本地存储适合开发测试
   - 生产环境建议使用 S3 兼容存储
   - 支持自动故障转移（优先本地，其次 S3）

8. **视频格式**:
   - 输入支持 MP4 格式
   - 输出为 HLS 格式（.m3u8 + .ts 文件）
   - 自动生成多分辨率版本

## 性能优化建议

1. **并发处理**: 通过 `permits` 参数控制并发转换任务数
2. **令牌桶速率**: 通过 `token_rate` 控制全局带宽
3. **缓存策略**: 使用 CDN 缓存静态视频文件
4. **存储选择**: 生产环境使用对象存储（S3）提高可扩展性
5. **CRF 调优**: 根据实际需求平衡质量和文件大小
6. **令牌限制**: 根据用户级别设置不同的速率限制

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
- Claim 令牌认证
- 速率限制功能
- S3 存储支持
- 灵活的令牌限制参数（全部可选）
