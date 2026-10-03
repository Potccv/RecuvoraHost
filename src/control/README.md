# 故障控制

本模块只维护 [IncidentLedger](recovery/incidents/README.md)：不可变 Node 错误收据、来源覆盖记录及接收检查点，并保留低层故障事实接口。它返回领域提案，由 [persistence](../persistence/README.md) 原子保存。错误识别归 Node，后续恢复流程归 Core。

审批、许可、恢复任务、知识隔离和业务阶段推进通过 Core 公共 API 使用。`binding`、`collections`、`identity` 是故障台账内部辅助；提交类型使用 Core `operation`。

导航见[恢复故障](recovery/README.md)，开发规则见[AGENTS](AGENTS.md)。
