# Host 恢复应用服务

本模块连接 Core 恢复引擎与 Host 的可信事实、持久化和运行生命周期。`RecoveryService` 提供查询、人工决定、任务驱动和关闭入口；`RecoveryEngine` 决定观察、审批、执行、验收及总结顺序，Host 不维护另一套业务阶段循环。

| 模块 | 职责 |
| --- | --- |
| `service` | 公开服务门面、目标绑定、调用监督与关闭顺序 |
| `aggregate` | 单一 RecoverySession 的打开、重放、可靠提交与 Host 派发历史 |
| `platform` | Core RecoveryPlatform 能力适配与有界调用 |
| `dispatch` | 具体动作提交、实际发送前复核及派发门释放 |
| `contract` | RepairBackend、AuthorizedRepair 与恢复专用 RepairActionGuard |
| `scheduler` | 当前故障转任务、驱动时间与串行运行实例 |
| `incident_gate`、`incident_guard` | 当前故障、样本时效、覆盖和派发期间的事实保护 |
| `ownership`、`storage_layout`、`storage_paths` | 稳定目标归属、物理文件身份及存储互斥 |

聚合与派发日志由同一个服务持有，在同一状态锁下访问。`recovery.jsonl` 原子确认审批、任务和经验相关变化；`dispatch.jsonl` 只证明 Host 实际派发边界。模块拆分不改变日志格式、锁范围或提交顺序，具体要求见[提交与历史](../../docs/control/commits.md)。

`RepairActionGuard` 负责已批准会话中的具体动作提交；通用网络 `DispatchGuard` 只提供发送前验证与释放。二者共享同一派发门，动作提交不能产生第二次执行许可。节点、Harness、执行器配置与总结输入适配见[恢复集成](../integrations/recovery/README.md)。旧的 `integrations::recovery` 公开导入路径继续重导出本模块服务。

关闭先排空调用并同步全部日志，再由 Core 判断是否可释放目标归属，最后关闭句柄；失败保留可重试状态。低层嵌入调用须显式绑定 TargetOwnership、IncidentGuard 并负责关闭，见[Rust API](../../docs/api/rust.md)。
