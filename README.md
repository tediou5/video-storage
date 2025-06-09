# video storage

## Cli

- -h, --help: Print help
- -l, --listen-on-port <LISTEN_ON_PORT>  [default: 32145]
- -p, --permits <PERMITS>                [default: 5]

## Upload video

curl -X POST "http://0.0.0.0:32145/upload?id=video&crf=48" --data-binary @video.mp4 -H "Content-Type: application/octet-stream"

- id: 不包含' ', '-', '/', '.'的唯一id
- crf: 0-63之间

## Get resource

http://127.0.0.1:32145/videos/video.m3u8

## Upload video object-file

curl -X POST 'http://127.0.0.1:32145/upload-objects?id=12345&name=test-file.tediou5' --header 'Content-Type: application/octet-stream' --data-binary '@test-file.tediou5'

- id: 不包含' ', '-', '/', '.'的唯一id
- name: 不包含' ', '/',

## Get video object-file

curl --fail -X GET http://127.0.0.1:32145/objects/12345/test-file.tediou5 -o downloaded-test-file.tediou5