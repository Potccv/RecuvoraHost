# Host Rust 嵌入接口

库名为 `recuvora_host`。下表按服务列出公开模块、调用入口及可扩展接口约定；请求/结果类型与完整方法签名以对应源码为准。

| 公开模块 | 接口与能力 |
| --- | --- |
| [`boot`](../../src/boot/README.md) | `run_cli`；`host::HostConfig`、`MonitoringHostConfig`、`HostRuntime::start`；`harnesses`/`extensions`/`monitoring` 获取共享服务；`start_recovery`、`recovery`、`recovery_scheduler`；`begin_shutdown`/`shutdown` |
| [`configuration`](../../src/configuration/README.md) | `RecoveryHostConfig::load`/`validate`、Harness 配置加载、独立文本修复配置加载/路径准备；`boot::host` 的加载包装增加应用源码边界及扩展控制路径保护 |
| [`server`](../../src/server/README.md) | `ServerConfig`、`LogSourceConfig`/`LogResultMapping`、`ApiError`；`Console::open` 返回 console 与模拟 engine；`router` 接入 Axum；`Console::wait_for_idle`/`shutdown`，engine 另行关闭 |
| [`harnesses`](../../src/harnesses/README.md) | `HarnessRegistryBuilder::register`/`build`；`HarnessRegistry::definitions`/`default_harness`/`list_projects`/`create_project`/`run`/关闭；提供方接口约定 `HarnessProvider`、`HarnessAdapterFactory`，受控工具接口约定 `HarnessToolHandler` |
| [`monitoring`](../../src/monitoring/README.md) | `MonitorsConfig`、监控/发现配置与快照；`ObservationSource`；`MonitorEngine::start_with_source`/`start_with_source_and_incident_config`/`handle`/`shutdown`；`MonitorHandle` 查询定义、快照与故障、确认、执行前故障复核及等待当前任务结束 |
| [`integrations`](../../src/integrations/README.md) | `extensions` 中的 `ExtensionsConfig`/`NetworkEndpoint`/`NodeSettings`/`ProtocolSettings`、`ExtensionRegistry::connect`/`connect_with_settings`/`call_read_only`/`ui_links`/`ui_links_catalog`/`refresh_ui_links`、声明/状态/接口归属查询与关闭，以及底层 `ExtensionClient`/`CallbackHandler`；`harness::RemoteHarnessFactory`；`monitoring::RegistryObservationSource`；`recovery::NodeRepairBackend`/`MonitorIncidentGuard`/`RecoveryScheduler`/`IncidentTrigger` |
| [`persistence`](../../src/persistence/README.md) | `ApprovalStore`、`IncidentStore`、`KnowledgeStore` 与 Host 存储限额；知识存储只登记完整修复经验；底层 `Journal` 只供可信宿主代码使用 |
| [`runtime`](../../src/runtime/README.md) | `Runtime::new`/`add`/`validate`/`start`/`snapshot`/`shutdown`；`Module`/`ModuleMetadata`、`ServiceKey`、`ModuleContext`、类型化服务、事件订阅与实例资源 |
| [`repair`](../../src/repair/README.md) | `RepairConfig`、`RepairSession::open`/`open_with_store_config`/`run`、记录查询、决定/撤销、带 revision 的认证决定/执行/结果核实；结果与错误类型 |
| [`actions`](../../src/actions/README.md) | `ScopedFiles::open`/`root`/`allowed_files`/`read`、`TextEdit`/`ActionReceipt`/`ActionError`；实际写入保持可信宿主内部边界 |
| [`simulation`](../../src/simulation/README.md) | `Engine::open`/`handle`/`submit`/`query`/`cancel`/`shutdown`、共享 `EngineHandle`、`EngineConfig`、`TaskSpec`/`TaskSnapshot`/`TaskState`/`Simulation` |

`HostRuntime::open_recovery(data_dir, config, executor)` 是低层 Host 恢复流程打开入口，显式接收 `ScriptExecutorConfig`，不启动恢复流程调度器；嵌入方需通过 `RecoveryService::bind_target_ownership` 绑定共享 `TargetOwnership`，绑定 `IncidentGuard`，并负责关闭恢复流程。未绑定目标所有权不能提交或推进任务；未绑定故障权威不能登记新故障。`start_recovery` 从必填 `ownership_dir` 打开 Host 的 `FileTargetOwnership` 并绑定，然后持有恢复流程调度器；关闭先取消并等待正在处理的恢复任务结束，再释放监控和提供方。保护同一规范目标的所有恢复存储必须共用稳定的所有权目录，不能随状态目录更换。嵌入方负责认证调用者和维护控制路径隔离，直接调用库不经过 HTTP 权限检查。

Host 定义 `RepairBackend` 与 `IncidentGuard`，通过 `persistence` 提供 `IncidentStore`、`ApprovalStore` 和知识命令存储；Core 的 `IncidentLedger`、`ApprovalLedger`、`KnowledgeState` 与 `RecoveryState` 计算变化，可靠提交后才确认提案并取得 `ExecutionPermit` 和后续意图；模型与插件拿不到核心状态存储或自行构造执行许可的入口。`cli` 与 `presentation` 是内部模块，外部应用使用上述公开服务。

协议类型与校验接口见 [Protocol Rust API](protocol.md)，应用职责与关闭顺序见 [架构](../architecture.md)。外部节点使用 [语言无关协议规范](../extensions/protocol.md)，不依赖本库。

当前存储格式见 [持久化](../../src/persistence/README.md)。所有领域提案只在可靠保存后确认；重放历史不得派发外部副作用。项目不提供旧事件日志导入接口，当前范围见 [HOST-003](../status.md#host-003)。

## 独立经验处理

`RepairBackend::summarize(job, config, cancellation)` 接收已提交结果快照并返回 `ExperienceReport`，不执行修复动作；默认实现返回不可用。`RecoveryService::summarize_pending` 推进自动预算内的总结和交付，`retry_experiences` 可在三次自动尝试后显式再试，`pending_experiences` 查询待处理工作，`experiences(&KnowledgeQuery)` 查询已提交经验。所有外部总结调用纳入 `CallScope`，关闭时取消并等待。

`NodeRepairBackend::new(harnesses, extensions, executor)` 显式接收 `ScriptExecutorConfig` 并返回 `Result`。该 Host 配置限定脚本平台、语言和只读检查查询；Core 只接收中立 `RepairArtifact`，不解释其脚本载荷。`RepairBackend::persistence_binding()` 必须返回影响派发的额外可信配置，包装后端须转发；内置后端返回执行器配置。恢复与派发日志头固定绑定该值，同一状态目录改变配置时拒绝打开，不能扩大原审批范围。

自定义后端只处理 `repair_with_harness`。`RepairBackend::execute` 接收 `AuthorizedRepair`，必须通过其 `dispatch_guard()` 的 `prepare_repair_action` 先持久保存具体中立动作，再在实际外部发送前调用 `validate_dispatch()`，发送完成或拒绝后调用 `finish_dispatch()`。动作绑定原 operation_id；内置节点适配器生成 `<operation_id>-action`、version 1，回执 `execution_trace` 与已保存动作完全一致。后端只能在可信委托内执行最多一次变更，不能把模型最终文本变成回执。
