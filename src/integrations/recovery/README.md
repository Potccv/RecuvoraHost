# 恢复能力适配

本目录实现[恢复应用服务](../../recovery/README.md)的外部能力。`node_backend.rs` 将节点和 Harness 映射为 RepairBackend，解析 node/workspace/endpoint 并收集独立结果证据；`executor.rs` 维护平台、语言和只读查询配置。

`harness_repair.rs` 实现受工具限制的修复会话及独立只读总结；`summary_context.rs` 构造最多 64 KiB 的总结输入。实际动作和可信结果必保留，可选字段整项省略并记录，发生省略时脚本化评估固定为 undetermined。完整 ExperienceJob 仍留在聚合历史。

具体动作通过 AuthorizedRepair 提供的恢复专用 RepairActionGuard 提交；网络客户端只接收通用 DispatchGuard，完成实际发送前复核与释放。业务聚合、调度、持久目标归属和关闭由 `crate::recovery` 维护，本目录保留旧 `integrations::recovery` 的公开重导出入口。

使用和限制见[恢复指南](../../../docs/recovery.md)，提交边界见[提交与历史](../../../docs/control/commits.md)。
