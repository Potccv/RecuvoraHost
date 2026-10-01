# Host 插件边界实现位置

外部扩展由 `RecuvoraHost` 统一连接并启动相关服务。

| 功能 | 当前实现 |
| --- | --- |
| v1 消息、ID、schema/value 校验 | `src/protocol/`；外部接入规范为 `docs/extension-protocol-v1.md` |
| HostRuntime、启动/关闭顺序、配置与保护路径 | `src/boot/host.rs` |
| endpoint、Bearer、TLS、HTTP/WebSocket 与协议会话 | `src/integrations/extensions/endpoint.rs`、`network.rs`、`transport.rs`、`client.rs` |
| 扩展配置和运行调优 | `src/integrations/extensions/config.rs`、`protocol_settings.rs`、`node_settings.rs` |
| ExtensionRegistry、声明校验、接口归属与路由 | `src/integrations/extensions/registry.rs`、`validation.rs`、`routing.rs` |
| 插件只读回调 | `src/integrations/extensions/callbacks.rs` |
| 插件声明式 view 注册及隔离容量 | `src/integrations/extensions/ui_view.rs` |
| 远端 Harness provider/factory 与工具回调 | `src/integrations/harness/` |
| ExtensionRegistry 到 ObservationSource 的适配 | `src/integrations/monitoring/mod.rs` |
| ExtensionRegistry/HarnessRegistry 到 RepairBackend 的适配 | `src/integrations/recovery/node_backend.rs` |
| Core 恢复流程配置、按配置启动，关闭时等待当前任务结束 | `src/configuration/recovery.rs`、`src/boot/host.rs` |
| 监控故障绑定、IncidentGuard 与恢复流程调度 | `src/integrations/recovery/incident_guard.rs`、`scheduler.rs` |
| Core 任务、审批、修复经验与根据节点证据核实结果的 HTTP 管理 | `src/server/recovery.rs` |
| 插件 UI catalog 与 view HTTP 接口 | `src/server/http.rs`、`src/server/monitoring.rs` |

当前插件 UI 接口为 `GET /api/v1/ui/catalog` 和 `GET /api/v1/ui/plugins/{plugin_id}/views/{view_id}`。首个 renderer 为 `monitoring_v1`，返回经 Host 校验和权限过滤的只读文档；旧 `/api/v1/monitoring/plugins/{id}` 保留为兼容接口。插件不能提交 contract、method 或任意 params，也不能用 view 声明获得写权限。

Host wire/schema 由本包 `protocol` 模块实现；节点/插件依据文档独立实现，不依赖宿主包。Core 仅接收稳定业务 trait 和领域类型，不感知 endpoint、ExtensionRegistry 或 UI capability。
