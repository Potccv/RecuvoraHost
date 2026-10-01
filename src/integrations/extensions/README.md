# 扩展运行时

本目录实现以下宿主基础设施：

- NetworkEndpoint 与可信配置解析。
- HTTP/WebSocket/TLS 会话和 Bearer 注入。
- ExtensionClient 与请求监督。
- ExtensionRegistry、接口归属、实例状态与关闭。
- 插件只读 service.call 回调和调用容量。
- 通用、无 UI 语义的隔离 descriptor 调用。

Host wire/schema 统一在本包 `src/protocol/`；监控、Harness 和修复的业务解释分别由相邻适配目录完成。
