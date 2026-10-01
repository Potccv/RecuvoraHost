# Host Rust 嵌入接口

库名为 `recuvora_host`。下表按服务列出公开模块、调用入口及可扩展接口约定；请求/结果类型与完整方法签名以对应源码为准。

| 公开模块 | 接口与能力 |
| --- | --- |
| [`boot`](../src/boot/README.md) | `run_cli`；`host::HostConfig`、`MonitoringHostConfig`、`HostRuntime::start`；`harnesses`/`extensions`/`monitoring` 获取共享服务；`start_recovery`、`recovery`、`recovery_scheduler`；`begin_shutdown`/`shutdown` |
| [`configuration`](../src/configuration/README.md) | `RecoveryHostConfig::load`/`validate`、Harness 配置加载、旧修复配置加载/路径准备；`boot::host` 的加载包装增加应用源码边界及扩展控制路径保护 |
| [`server`](../src/server/README.md) | `ServerConfig`、`LogSourceConfig`/`LogResultMapping`、`ApiError`；`Console::open` 返回 console 与模拟 engine；`router` 接入 Axum；`Console::wait_for_idle`/`shutdown`，engine 另行关闭 |
| [`harnesses`](../src/harnesses/README.md) | `HarnessRegistryBuilder::register`/`build`；`HarnessRegistry::definitions`/`default_harness`/`list_projects`/`create_project`/`run`/关闭；提供方接口约定 `HarnessProvider`、`HarnessAdapterFactory`，受控工具接口约定 `HarnessToolHandler` |
| [`monitoring`](../src/monitoring/README.md) | `MonitorsConfig`、监控/发现配置与快照；`ObservationSource`；`MonitorEngine::start_with_source`/`start_with_source_and_incident_config`/`handle`/`shutdown`；`MonitorHandle` 查询定义、快照与故障、确认、执行前故障复核及等待当前任务结束 |
| [`integrations`](../src/integrations/README.md) | `extensions` 中的 `ExtensionsConfig`/`NetworkEndpoint`/`NodeSettings`/`ProtocolSettings`、`ExtensionRegistry::connect`/`connect_with_settings`/`call_read_only`、声明/状态/接口归属查询与关闭，以及底层 `ExtensionClient`/`CallbackHandler`；`harness::RemoteHarnessFactory`；`monitoring::RegistryObservationSource`；`recovery::NodeRepairBackend`/`MonitorIncidentGuard`/`RecoveryScheduler`/`IncidentTrigger` |
| [`persistence`](../src/persistence/README.md) | `ApprovalStore`、`IncidentStore`、`KnowledgeStore` 与 Host 存储限额；`legacy::import_legacy_journal` 转换完整旧事件日志，`legacy::import_legacy_recovery_bundle` 校验并原子安装完整旧恢复存储；底层 `Journal` 只供可信宿主代码使用 |
| [`runtime`](../src/runtime/README.md) | `Runtime::new`/`add`/`validate`/`start`/`snapshot`/`shutdown`；`Module`/`ModuleMetadata`、`ServiceKey`、`ModuleContext`、类型化服务、事件订阅与实例资源 |
| [`repair`](../src/repair/README.md) | `RepairConfig`、`RepairSession::open`/`open_with_store_config`/`run`、记录查询、决定/撤销、带 revision 的认证决定/执行/结果核实；结果与错误类型 |
| [`actions`](../src/actions/README.md) | `ScopedFiles::open`/`root`/`allowed_files`/`read`、`TextEdit`/`ActionReceipt`/`ActionError`；实际写入保持可信宿主内部边界 |
| [`simulation`](../src/simulation/README.md) | `Engine::open`/`handle`/`submit`/`query`/`cancel`/`shutdown`、共享 `EngineHandle`、`EngineConfig`、`TaskSpec`/`TaskSnapshot`/`TaskState`/`Simulation` |

`HostRuntime::open_recovery` 是低层 Host 恢复流程打开入口，不启动恢复流程调度器；嵌入方需通过 `RecoveryService::bind_target_ownership` 绑定共享 `TargetOwnership`，绑定 `IncidentGuard`，并负责关闭恢复流程。未绑定目标所有权不能提交或推进任务；未绑定故障权威不能登记新故障。`start_recovery` 从必填 `ownership_dir` 打开 Host 的 `FileTargetOwnership` 并绑定，然后持有恢复流程调度器；关闭先取消并等待正在处理的恢复任务结束，再释放监控和提供方。保护同一规范目标的所有恢复存储必须共用稳定的所有权目录，不能随状态目录更换。嵌入方负责认证调用者和维护控制路径隔离，直接调用库不经过 HTTP 权限检查。

Host 定义 `RepairBackend` 与 `IncidentGuard`，通过 `persistence` 提供 `IncidentStore`、`ApprovalStore` 和知识命令存储；Core 的 `IncidentLedger`、`ApprovalLedger`、`KnowledgeState` 与 `RecoveryState` 计算变化，可靠提交后才确认提案并取得 `ExecutionPermit` 和后续意图；模型与插件拿不到核心状态存储或自行构造执行许可的入口。`cli` 与 `presentation` 是内部模块，外部应用使用上述公开服务。

协议类型与校验接口见 [Protocol Rust API](protocol-rust-api.md)，应用职责与关闭顺序见 [架构](architecture.md)。外部节点使用 [语言无关协议规范](extension-protocol-v1.md)，不依赖本库。

Core 0.2 的存储格式及离线导入前提见 [持久化](../src/persistence/README.md)。所有领域提案只在可靠保存后确认；重放历史不得派发旧副作用。旧工作流身份保留、安装顺序与导入限制见 [HOST-003](implementation-status.md#host-003)。
