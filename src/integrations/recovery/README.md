# 远端修复后端适配

`node_backend.rs` 实现 Core `RepairBackend`：节点 inspect/verify/execute_script 映射、诊断工具和独立审核会话。`check_task_result` 从原执行节点分别调用节点的只读 reconcile 和 verify，再调用 RecoveryService.check_result，由 Core 校验并保存结果；不接受客户端提交执行事实，不重放动作。

`incident_guard.rs` 实现 Core `IncidentGuard`，在 Host 监控登记同步锁内复核当前故障、绑定、覆盖及单调新鲜度。`scheduler.rs` 只负责固定触发、串行调度、去重和取消并等待当前任务结束，任务状态由 Core 决定。

HostRuntime.start_recovery 向 Core 提供上述接口实现，并管理恢复流程调度器。Core 保留 AuthorizedScript、ScriptReceipt/ScriptOutcome、恢复流程状态机、审批、许可和保存在磁盘上的记录；node/workspace/endpoint 解析只在 Host。配置与 HTTP 见[恢复流程说明](../../../docs/recovery.md)。

Host 的 `RecoveryService` 在 `service.rs` 中提供配置、任务、审批、知识库、恢复与 `check_result` 接口，内部调用 `recuvora_core::recovery::workflow::RecoveryService`；恢复判定和记录存储由 Core 实现。配置、任务、阶段与执行结果核实直接使用 Core 的 `RecoveryConfig`、`RecoveryTask`、`RecoveryStage` 和 `ExecutionResultCheck`。低层打开服务只允许查询，提交和推进前须绑定 `TargetOwnership`，新故障登记还须绑定 `IncidentGuard`。HostRuntime 从显式 `ownership_dir` 打开 Core `FileTargetOwnership` 并绑定；所有保护同一规范目标的存储共享该目录。关闭时 Core 只在任务全部终结且无 Executing/Unknown 审批后释放持久所有权，未完成或 Unknown 工作继续阻断其他状态目录；同一存储可恢复自己的所有权。
