# Host 架构

Node 识别错误并提供错误日志，Host 校验、持久接收并将完整错误报告交给 Core。Core 主导后续恢复流程；Host 提供持久化、Harness/节点能力、认证及运行生命周期。依赖方向是 Host → Core，运行时由 Core 调用 Host 注入的能力。

## 模块职责

| 组件 | 责任 |
| --- | --- |
| Core RecoveryEngine / RecoverySession | 经验匹配、审批、执行编排、验收判定、经验生成与原子领域提案 |
| Host [control](../src/control/README.md) | IncidentLedger 不可变错误收件、来源覆盖记录与读取检查点 |
| Host persistence | 配置和日志绑定、版本比较、可靠同步、提交确认 |
| Host [application](../src/application/README.md) | 服务所有权、应用操作受理、持久回执、去重、容量、取消及关闭 |
| Host [recovery](../src/recovery/README.md) | 实现 Core 能力接口，管理聚合日志、收件绑定、实际派发保护及受管调度 |
| Host boot/configuration | 可信装配、配置加载与控制路径保护 |
| Host runtime | 通用取消与在途监督，以及可信模块的生命周期框架 |
| Host protocol/integrations | 有界网络消息、schema、TLS、路由、节点和 Harness 适配 |
| Node/插件 | Node 采集和识别错误、提供错误日志、动作执行、执行者监督与独立业务验收；插件提供契约及展示描述 |
| UI/CLI/HTTP | 认证后的查询与管理请求，不产生权威事实 |

## 应用依赖与装配

boot 是可信装配入口，先由 configuration 读取并校验部署配置，再构建应用服务、恢复服务和具体适配器。application 持有共享服务及运行生命周期；server 只保留 HTTP 身份、权限、请求转换、静态资产与响应投影。操作日志与调用取消由 application 管理，HTTP 请求结束不改变其所有权。

configuration 不依赖 boot，application 不依赖 server 或 Axum。recovery 通过 RepairBackend 使用外部能力，不导入具体 NodeRepairBackend；integrations 的具体后端依赖 recovery 契约。通用网络 DispatchGuard 只执行最终发送复核和释放，恢复专用 RepairActionGuard 负责在已批准会话内准备具体动作。

独立本机文本修复与封闭模拟是单独的应用能力，不进入 recovery 的领域聚合。它们保留既有接口、配置和记录语义；新恢复能力通过 Core、recovery 和外部后端扩展。通用 Module 框架供可信嵌入和模拟命令使用，生产共享服务由 HostRuntime 显式持有。

## 恢复接口

`RecoveryService::advance` 在监督范围内调用 Core `RecoveryEngine::advance`。私有 Platform 实现 `RecoveryPlatform`，不包含恢复阶段循环。Core 发起观察、审核、修复、验收和总结，Host 使用 `RepairBackend` 完成实际能力调用并返回证据。

`RecoverySession` 的完整提案保存到单一 `recovery.jsonl`；审批和任务的相关变化一起确认。恢复模块内部将聚合存储、Core Platform、派发门和公开服务分开实现，仍共享同一聚合所有者与提交边界。`dispatch.jsonl` 单独保存 Host 的物理派发边界；它不参与业务阶段决策，只为中断后独立证明尚未派发提供依据。具体要求见[提交与历史](control/commits.md)。

错误收件与游标在同一事务保存；每条日志有独立稳定身份，同身份改写拒绝。收件后唤醒恢复调度，将原文、来源身份及原始证据传入 Core 的 ErrorLog 问题报告；Core 忙时留存待交付，重启按原身份继续，不重复创建任务。Host 没有健康规则、错误级别筛选或日志文本诊断。HTTP 日志查询只读取同一持久收件，不另外从节点采集。

Node 保证上报的是已识别的实时错误，Host/Core 不再查询或判断日志是否活跃。Host 保持收件身份保护、目标所有权和最终网络发送复核；Core 接收报告后进入既有经验匹配与恢复流程。目标观察采集环境和执行前提，不用于复判日志活跃状态；审批、一次许可和独立业务验收继续约束执行。空错误批次和来源失联不清除收件或证明恢复。

## 服务入口和关闭

`start_recovery` 按显式配置启动调度并绑定稳定 `ownership_dir`。低层 `open_recovery(data_dir, config, executor)` 只打开服务，嵌入方负责绑定 `TargetOwnership`、`IncidentGuard`、认证与关闭，见[Rust API](api/rust.md)。

关闭先停止新请求和调度，再请求取消并排空在途调用；同步全部日志，确认 Core 聚合可以释放后才释放持久归属和文件句柄。同步或释放失败保留句柄，允许重试。断连、取消及等待超时不证明执行者停止；Unknown 或不确定提交保留目标归属。

同一实际目标必须使用相同规范身份与稳定所有权目录，不能更换恢复目录绕过未结束任务。独立文本修复和自动恢复不得同时管理同一目标。
