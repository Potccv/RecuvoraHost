# Host 架构

Core 主导恢复业务流程；Host 提供故障、可信事实、持久化、Harness/节点能力、认证及运行生命周期。依赖方向是 Host → Core，运行时由 Core 调用 Host 注入的能力。

## 模块职责

| 组件 | 责任 |
| --- | --- |
| Core RecoveryEngine / RecoverySession | 经验匹配、审批、执行编排、验收判定、经验生成与原子领域提案 |
| Host [control](../src/control/README.md) | IncidentLedger 故障台账与监控检查点 |
| Host persistence | 配置和日志绑定、版本比较、可靠同步、提交确认 |
| Host RecoveryService | 实现 Core 能力接口，管理聚合日志、当前故障门、实际派发保护及监督调用 |
| Host runtime/boot | 装配、配置、调度、取消与关闭 |
| Host protocol/integrations | 有界网络消息、schema、TLS、路由、节点和 Harness 适配 |
| Node/插件 | 实际采集、动作执行、执行者监督与独立业务验收 |
| UI/CLI/HTTP | 认证后的查询与管理请求，不产生权威事实 |

## 恢复接口

`RecoveryService::advance` 在监督范围内调用 Core `RecoveryEngine::advance`。私有 Platform 实现 `RecoveryPlatform`，不包含恢复阶段循环。Core 发起观察、审核、修复、验收和总结，Host 使用 `RepairBackend` 完成实际能力调用并返回证据。

`RecoverySession` 的完整提案保存到单一 `recovery.jsonl`；审批和任务的相关变化一起确认。`dispatch.jsonl` 单独保存 Host 的物理派发边界；它不参与业务阶段决策，只为中断后独立证明尚未派发提供依据。具体要求见[提交与历史](control/commits.md)。

Host 保持当前故障保护、目标所有权和最终网络发送复核；Core 的许可不替代物理保护。模型只提供建议，实际执行和独立验收事实经 Core 判定后形成结果。业务阶段与权威规则由 Core 维护，Host 不复制这些实现。

## 服务入口和关闭

`start_recovery` 按显式配置启动调度并绑定稳定 `ownership_dir`。低层 `open_recovery(data_dir, config, executor)` 只打开服务，嵌入方负责绑定 `TargetOwnership`、`IncidentGuard`、认证与关闭，见[Rust API](api/rust.md)。

关闭先停止新请求和调度，再请求取消并排空在途调用；同步全部日志，确认 Core 聚合可以释放后才释放持久归属和文件句柄。同步或释放失败保留句柄，允许重试。断连、取消及等待超时不证明执行者停止；Unknown 或不确定提交保留目标归属。

同一实际目标必须使用相同规范身份与稳定所有权目录，不能更换恢复目录绕过未结束任务。独立文本修复和自动恢复不得同时管理同一目标。
