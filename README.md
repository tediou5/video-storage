# video storage

## Cli

```text
Usage: video-storage [OPTIONS]

Options:
  -l, --listen-on-port <LISTEN_ON_PORT>
          [default: 32145]
  -p, --permits <PERMITS>
          [default: 5]
  -t, --token-rate <TOKEN_RATE>
          [default: 0]
  -w, --workspace <WORKSPACE>
          [default: .]
  -s, --storage-backend <STORAGE_BACKEND>
          Storage backend: local or s3 [default: local]
      --s3-bucket <S3_BUCKET>
          S3 bucket name (required when storage-backend is s3)
      --s3-endpoint <S3_ENDPOINT>
          S3 endpoint (for MinIO/custom S3)
      --s3-region <S3_REGION>
          S3 region
      --s3-access-key-id <S3_ACCESS_KEY_ID>
          S3 access key ID
      --s3-secret-access-key <S3_SECRET_ACCESS_KEY>
          S3 secret access key
  -h, --help
          Print help
  -V, --version
          Print version
```

for example:

```shell
video-storage -p 1 \
-s s3 \
  --s3-bucket video-storage \
  --s3-endpoint http://127.0.0.1:9000 \
  --s3-region us-east-1 \
  --s3-access-key-id minioadmin \
  --s3-secret-access-key minioadmin
```

## Check waitlist len

Return the length of convert jobs waitlist.

example:

```shell
curl --fail -X GET http://127.0.0.1:32145/waitlist
6
```

## Upload video

```shell
curl -X POST "http://0.0.0.0:32145/upload?id=video&crf=48" --data-binary @video.mp4 -H "Content-Type: application/octet-stream"
```

- id: 不包含' ', '-', '/', '.'的唯一id
- crf: 0-63之间

## Get resource

```shell
http://127.0.0.1:32145/videos/video.m3u8
```

## Upload video object-file

```shell
curl -X POST 'http://127.0.0.1:32145/upload-objects?id=12345&name=test-file.tediou5' --header 'Content-Type: application/octet-stream' --data-binary '@test-file.tediou5'
```

- id: 不包含' ', '-', '/', '.'的唯一id
- name: 不包含' ', '/',

## Get video object-file

```shell
curl --fail -X GET http://127.0.0.1:32145/objects/12345/test-file.tediou5 -o downloaded-test-file.tediou5
```
