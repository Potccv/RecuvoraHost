# HTTP 应用服务

本模块将 [Host 应用服务](../application/README.md)映射为带 Bearer 认证的 `/api/v1`，维护明确允许的浏览器来源、权限、revision 与摘要/详情。CLI 与 HTTP 共用 Host 服务接口，HTTP 不通过命令行解析器调用业务。`Console` 是兼容 HTTP 门面，显式持有 `Application`；能力配置和控制路径保护由 [boot/application.rs](../boot/application.rs) 装配，操作回执、去重、容量、取消和已接受任务的运行由 application 维护。

服务仅监听回环，单操作员令牌由外部文件提供，operator 来自可信服务配置。审批和故障确认在负责最终判定的服务内核验 revision；UI按钮、插件输出或客户端提交的文本不能充当身份或执行许可。

`monitoring.rs` 的 UI catalog 同时返回声明式 views、插件 external_links 和 link_statuses。目录要求 extension.read，监控 views 额外按 monitor.read 过滤；读取快照不调用插件。`plugin_pages.rs` 提供正文为 `{}` 的显式页面描述刷新，只要求 extension.read，状态失败在有界快照中保留，容量忙拒绝而不排队。Host 不提供跳转按钮、不获取或代理插件网页，客户端工作及导航安全规则见[页面契约](../../docs/extensions/pages.md)。

`recovery.rs` 使用 RecoveryService，提供状态、分页任务、原始任务/审批详情、人工决定、paused 恢复、根据节点证据核实未知执行结果和修复经验搜索。结果核实由 application 复用启动时绑定的 NodeRepairBackend，HTTP 不创建后端或选择执行器。任务直接序列化 Core 的 `RecoveryTask`，执行结果核实记录使用 `result_check`，证据时间使用 `checked_at_ms`。recovery_config 显式启用调度并要求共享 ownership_dir，本机文本修复接口独立维护；同一 target 不允许同时配置两套恢复决策服务。恢复状态和共享所有权目录彼此隔离，均与服务/本机文本修复状态、控制文件和 TLS 信任文件隔离，并加入本机文本修复保护范围。

`project_logs.rs` 只投影同一 IncidentStore 中已持久接收的 Node 错误回执，以不可变回执 ID 有界分页。GET 不调用 Node、不筛选日志级别、不推进接收检查点或提交 Core 任务；Node 断连后仍可读取已有错误，空页不代表健康。字段与游标语义见[HTTP 只读记录](../../docs/api/http.md#只读视图与记录)。

application 的操作日志记录请求接受与结果，用稳定ID阻止重复派发；它不能替代 Core 审批、故障和任务记录。连接结束、读取超时与请求取消均不能证明外部动作未发生，未知结果保持Unknown。

可选ui_dir启动时加载外部构建目录的14份固定静态文件为内存快照，单文件最多2 MiB、合计8 MiB。无配置返回404，不提供未知文件、目录浏览或任意路径；不含UI源码、构建或Tauri。该目录属于可信部署输入并受修复保护，没有安装签名验证或热更新机制。

路由、认证和持久边界见[HTTP说明](../../docs/api/http.md)，配置见[配置说明](../../docs/configuration.md)，规范见 [AGENTS](AGENTS.md)。
