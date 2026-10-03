# 故障控制

本模块只维护 [IncidentLedger](recovery/incidents/README.md)：当前故障、活动状态、观察与监控检查点。它返回领域提案，由 [persistence](../persistence/README.md) 原子保存。

审批、许可、恢复任务、知识隔离和业务阶段推进通过 Core 公共 API 使用。`binding`、`collections`、`identity` 是故障台账内部辅助；提交类型使用 Core `operation`。

导航见[恢复故障](recovery/README.md)，开发规则见[AGENTS](AGENTS.md)。
