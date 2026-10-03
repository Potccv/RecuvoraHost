# Host 架构

Host 集中管理认证、故障台账、审批、执行许可、恢复流程、知识隔离、持久化和运行生命周期。Core 只提供经验匹配、修复请求生成和经验构造；Node/插件采集目标、执行动作并提供独立验收。

## 模块职责

| 组件 | 责任 |
| --- | --- |
| Core 公共业务 API | `matching_experiences`、`prepare_repair`、`build_experience`，返回普通业务数据 |
| Host [control](../src/control/README.md) | 输入与政策校验、故障/审批/恢复/知识权威状态、一次许可、隔离、提交绑定、重放 |
| Host persistence | 配置和日志绑定、原子版本比较、可靠同步与提交确认 |
| Host RecoveryService | 协调 control、持久提交、目标所有权、当前故障门及外部调用 |
| Host runtime/boot | 服务装配、配置、定时处理、取消监督与关闭 |
| Host protocol/integrations | 有界网络消息、schema、TLS、路由、节点和 Harness 适配 |
| Node/插件 | 实际采集、动作、执行者监督、独立业务验收 |
| UI/CLI/HTTP | 经认证服务查询与提交管理请求，不自行产生授权事实 |

## 业务与管理接口

`control::recovery::workflow::RecoveryState` 在准备修复时调用 Core `prepare_repair`。知识管理调用 Core `matching_experiences`，经验交付从已提交任务提取结果、动作和验收证据后调用 Core `build_experience`。Host 重导出共享业务类型，以保持各服务使用同一数据定义。

Core 输出没有权限效力。Host 校验当前故障、环境、政策和审批期限，先可靠保存审批消费，再保存恢复授权；确认两项事实后才交付后端许可。具体动作提交后，实际网络发送前继续复核条件。细则见[审批](approval.md)和[提交约束](control/commits.md)。

`IncidentLedger`、`ApprovalLedger`、`KnowledgeState` 与 `RecoveryState` 位于 Host control，状态没有可绕过校验的反序列化入口。`CommitReceipt` 只能由可信持久层在可靠提交后确认。恢复历史重复合法性检查，不产生新许可或派发动作。恢复服务保留跨日志提交顺序及中断续接，当前没有跨日志统一事务。

## 服务入口和关闭

`start_recovery` 按显式配置启动恢复调度，并从稳定 `ownership_dir` 绑定共享目标所有权。`open_recovery(data_dir, config, executor)` 仅打开低层服务；嵌入方负责绑定 `TargetOwnership`、`IncidentGuard`、认证与关闭，见 [Rust API](api/rust.md)。

关闭先停止新请求和调度，再请求取消并等待恢复、监控和远端调用，保存最终事实后释放服务和文件锁。等待超过 30 秒保留错误和管理权，允许继续等待。断连或取消不证明节点执行者已停止；Unknown 和不确定提交保留目标归属。

同一实际目标使用相同规范身份和稳定所有权目录，不得以更换恢复目录绕过未结束任务或 Unknown。独立文本修复与自动恢复使用独立入口，禁止同时管理同一目标。具体限制见[恢复流程](recovery.md)。
