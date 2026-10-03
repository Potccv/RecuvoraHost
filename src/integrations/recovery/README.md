# 恢复能力适配

`RecoveryService` 管理 Host 的聚合存储、目标所有权、故障保护和监督调用；其私有 Platform 实现 Core RecoveryPlatform。RecoveryEngine 决定观察、审批、执行、验收和总结顺序，本目录不维护业务阶段循环。

`service.rs` 将 RecoverySession 的完整提案保存到 recovery.jsonl。审批、任务和经验相关变化统一确认；dispatch.jsonl 只维护实际派发边界。中断、重开和文件限额见[提交与历史](../../../docs/control/commits.md)。

`contract.rs` 定义 Host RepairBackend 和受许可保护的 AuthorizedRepair，审核及验收输入重导出自 Core。`node_backend.rs` 解析 node/workspace/endpoint，提供实际能力；`executor.rs` 维护平台、语言和只读查询配置。

`harness_repair.rs` 实现受工具限制的修复会话及独立只读总结；`summary_context.rs` 构造最多 64 KiB 的总结输入。实际动作和可信结果必保留，可选字段整项省略并记录，发生省略时脚本化评估固定为 undetermined。完整 ExperienceJob 仍留在聚合历史。

`incident_gate` 与 `incident_guard` 保持活动故障、样本时效、覆盖与实际发送的保护。`ownership`、`storage_paths` 和 `storage_layout` 维护物理文件身份、互斥和稳定归属。关闭先排空、同步，再根据 Core releasable 释放归属，最后关闭句柄；失败保持可重试。

`RecoveryScheduler` 只安排驱动时间和运行实例，不决定业务下一阶段。使用和限制见[恢复指南](../../../docs/recovery.md)。
