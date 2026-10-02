# Host 架构

Host 是统一服务启动入口。它读取配置、认证操作员，初始化 runtime、监控、AI 服务接入（Harness）和 ExtensionRegistry，通过 Host 的 `RepairBackend`、`IncidentGuard` 调用节点与监控，并将证据提交给 Core 领域状态机。

## 职责

| 组件 | 责任 |
| --- | --- |
| Core | `operation` 提供提交提案与确认契约；`recovery` 计算故障、审批、一次性执行许可、修复经验与恢复流程的合法迁移 |
| Host | 加载配置、身份认证、持久化、原子提交、取消监督、目标所有权、定时处理监控数据、选择 Harness 和执行节点、提供 HTTP/CLI |
| Host `protocol` 模块 | 实现宿主的 v1 消息、schema、身份、版本、限额和 ID 规则；外部节点按 Host 文档独立实现 |
| Node/插件 | 检查目标、执行动作、监督执行者、独立业务验收；提供自定义只读业务接口 |
| UI | 通过 HTTP 查询和展示记录，提交人工决定；权限、执行结果和恢复状态由服务判定 |

Core 当前没有 HostSettings/CoreSettings、网络客户端、HarnessRegistry、MonitorEngine 或原始日志上传接口。

## Core 接口映射

| 服务接口 | 实现与使用位置 |
| --- | --- |
| Host `runtime::operation::{Cancellation, CallScope}` | 管理 Harness、扩展、监控和恢复流程调用的取消与结束等待 |
| Host `persistence::incidents::IncidentStore` / Core `IncidentLedger` | 同时保存观察读取位置和故障，全部成功或全部失败；HTTP 查询和确认故障时核对记录版本号（revision） |
| Host `ApprovalStore` / Core `ApprovalLedger`、`ExecutionPermit` | 自动恢复流程使用审批与一次性许可；独立文本修复复用审批服务 |
| `RepairBackend` | `NodeRepairBackend` 实现 inspect/review/execute/verify/summarize；具体脚本类型与 executor 配置由 Host 解释 |
| `IncidentGuard` | `MonitorIncidentGuard` 在监控同步锁内核对故障是否仍有效 |
| `TargetOwnership` | HostRuntime 从显式共享 ownership_dir 打开 Host 的 FileTargetOwnership，绑定规范目标与恢复存储；非终态、Unknown 和不确定提交的持久所有者由 Host 保留 |
| Host `RecoveryService` / Core `RecoveryState` | HostRuntime 按配置启动；HTTP 使用 query/tasks/approval/decide_human/resume/check_result/knowledge |
| `RecoveryTask`、`ApprovalRecord`、`RepairExperience` | 恢复流程详情由 Core 记录生成，结果核实记录通过 result_check 展示；摘要省略完整脚本与审批规则 |

`submit` 和 `advance` 由恢复流程调度器按固定故障触发规则调用。客户端不能提交故障事实、脚本、审批规则或验收结论。暂停任务通过 RecoveryService.resume 恢复，未知执行结果（Unknown）通过 Host check_result 接口核实。`/repairs`、`/approvals` 是本机文本修复接口，自动恢复接口位于 `/recovery`。

Core 根据逻辑 `execution_harness`、`executor_id` 和允许的动作种类判定并记录完整 Harness 修复委托；具体动作使用中立 `RepairArtifact`。Host 的 `ScriptExecutorConfig` 管理脚本平台、语言和检查查询，并解析实际 node、workspace、endpoint 和连接。Node/插件返回检查、执行和验收证据，Core 据此决定最终状态。

## 关闭服务

关闭顺序为：停止接受新的 HTTP/CLI 请求与定时派发；请求取消并等待恢复流程、监控和远端调用结束；保存最终结果；释放 Harness、扩展、runtime 和文件锁。连接关闭不能证明节点上的执行者已经停止。

`start_recovery` 由 HostRuntime 绑定共享目标所有权并管理恢复流程调度器，关闭时先等待恢复任务结束，再停监控和外部服务。等待超过 30 秒时保留错误和服务管理权，允许再次等待。`open_recovery(data_dir, config, executor)` 显式接收 Host 执行器配置，只打开 Host 存储与 Core 领域状态，不启动调度器；调用方负责绑定 TargetOwnership、IncidentGuard 并关闭服务。所有保护同一规范目标的恢复存储必须共用稳定所有权目录，未完成和 Unknown 不能通过更换状态目录绕过互斥。

人工批准、拒绝、撤销与结果核实必须经过身份认证的 Host API，并由 Core 校验。
