# 恢复控制

本模块集中管理 Host 权威状态；业务计算调用 `recuvora_core`，不重复实现经验匹配或请求预算选择。

| 位置 | 责任 |
| --- | --- |
| [operation.rs](operation.rs) | 完整输入和版本绑定的提案、提交确认及效果交付 |
| [approval](recovery/approval/README.md) | 硬政策、审核来源、期限、一次许可、同目标互斥 |
| [incidents](recovery/incidents/README.md) | 故障与检查点事务、活动事实、完整历史 |
| [knowledge](recovery/knowledge/README.md) | 可信经验、不可变动作版本、隔离与幂等记录 |
| [workflow](recovery/workflow/README.md) | 恢复阶段、当前证据、结果核实、总结与交付状态 |

`binding`、`collections` 与 `identity` 是内部辅助。control 计算待提交变化；[persistence](../persistence/README.md) 和[恢复服务](../integrations/recovery/README.md)负责实际日志、同步和外部调用。可靠确认后才安装新状态和取得效果。

共享的 `RepairArtifact`、故障上下文、描述性约束和经验报告重导出自 Core。可构造这些数据不意味着可获得 `ExecutionPermit`、`AuthorizedRepair` 或安装权威状态。

详细顺序见[控制参考](../../docs/control/README.md)。规范见 [AGENTS](AGENTS.md)。
