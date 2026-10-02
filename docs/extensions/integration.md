# Host 插件边界实现位置

外部扩展由 `RecuvoraHost` 统一连接并启动相关服务。

| 功能 | 当前实现 |
| --- | --- |
| v1 消息、ID、schema/value 校验 | `src/protocol/`；外部接入规范为 `docs/extensions/protocol.md` |
| HostRuntime、启动/关闭顺序、配置与保护路径 | `src/boot/host.rs` |
| endpoint、Bearer、TLS、HTTP/WebSocket 与协议会话 | `src/integrations/extensions/endpoint.rs`、`network.rs`、`transport.rs`、`client.rs` |
| 扩展配置和运行调优 | `src/integrations/extensions/config.rs`、`protocol_settings.rs`、`node_settings.rs` |
| ExtensionRegistry、声明校验、接口归属与路由 | `src/integrations/extensions/registry.rs`、`validation.rs`、`routing.rs` |
| 插件只读回调 | `src/integrations/extensions/callbacks.rs` |
| 插件声明式 view 注册及隔离容量 | `src/integrations/extensions/ui_view.rs` |
| 独立页面描述、快照与部署 URL 校验 | `src/integrations/extensions/ui_links.rs`、`page_urls.rs` |
| 远端 Harness provider/factory 与工具回调 | `src/integrations/harness/` |
| ExtensionRegistry 到 ObservationSource 的适配 | `src/integrations/monitoring/mod.rs` |
| ExtensionRegistry/HarnessRegistry 到 RepairBackend 的适配与 Host 脚本解释 | `src/integrations/recovery/node_backend.rs`、`executor.rs`、`harness_repair.rs` |
| Host 恢复服务配置、按配置启动及等待在途任务结束 | `src/configuration/recovery.rs`、`src/boot/host.rs` |
| 监控故障绑定、IncidentGuard 与恢复流程调度 | `src/integrations/recovery/incident_guard.rs`、`scheduler.rs` |
| Core 任务、审批、修复经验与根据节点证据核实结果的 HTTP 管理 | `src/server/recovery.rs` |
| 插件 UI catalog、view 与页面刷新 HTTP 接口 | `src/server/http.rs`、`src/server/monitoring.rs`、`src/server/plugin_pages.rs` |

插件目录通过 `GET /api/v1/ui/catalog` 提供声明式监控视图及独立页面入口，字段与刷新接口见[页面契约](pages.md)。`GET /api/v1/ui/plugins/{plugin_id}/views/{view_id}` 的 renderer 为 `monitoring_v1`，返回经 Host 校验和权限过滤的只读文档；旧 `/api/v1/monitoring/plugins/{id}` 保留为兼容接口。页面和 view 描述均不允许选择任意服务调用，也不产生写权限。

Host wire/schema 由本包 `protocol` 模块实现；节点/插件依据文档独立实现，不依赖宿主包。Core 仅接收逻辑身份、中立动作产物和领域类型，不感知 endpoint、ExtensionRegistry 或 UI capability。
