# 恢复状态机实现

`RecoveryState` 为一个逻辑目标计算恢复任务，不持有文件、时钟、后端、所有权锁或异步任务。公开入口由 `recovery::workflow` 导出。

| 文件 | 职责 |
| --- | --- |
| [contract.rs](contract.rs) | 配置、任务、执行与独立业务验收证据 |
| [engine.rs](engine.rs) | 命令、历史事件、提交提案、流程迁移与动作隔离 |
| [experience.rs](experience.rs) | 重导出业务请求；管理独立经验任务并调用 Core 构造经验 |
| [query.rs](query.rs) | `RecoveryTaskSummary` 与 `RecoveryNextStep` 结构化进度 |

## 统一修复入口

`RecoveryCommand::StartRepair` 使用稳定条件检索经验：一致合并可信 `ProblemContext.conditions` 与 `TargetBinding.required_facts`，调用 Core 合并并注入 `fault_fingerprint`。同名条件值冲突或输入占用该保留键会被拒绝；当前观察必须满足稳定条件，但额外观察 facts 只完整保留为请求证据，不自动扩大经验适用条件。

control 借用知识记录并调用 Core `prepare_repair`；Core 稳定匹配、排序，再按顺序整条选入最多四条参考经验。`HarnessRepairRequest.matched_experience_count` 保存完整命中数，`experiences` 只保存实际附带项，因此可区分没有命中与命中项全部因预算省略。完整请求 JSON 不超过 `MAX_REPAIR_REQUEST_BYTES`（32 KiB）；超预算条目省略，后续较小条目仍可选入。必需故障及观察本身超预算时返回错误且原状态不变。有经验和无经验都采用同一受授权 Harness 会话。配置版本为 2，政策必须显式允许 `repair_with_harness`。`TargetBinding.allowed_action_kinds` 限定会话内具体动作种类，Host control 不解释动作 payload 的平台、语言或提供方实现。

`RepairActionPrepared { action }` 在发送前保存完整 `RepairArtifact`，每个会话最多一个动作。动作 ID 为原操作 ID 加 `-action`，版本为 1，生成 Harness 和会话分别绑定配置身份与原操作 ID。校验内容不可变、当前观察满足前提、动作范围和永久隔离。`repair_action(operation_id)` 只读访问已提交动作，不产生许可；`RepairReceipt.execution_trace` 必须与该动作精确一致，`RepairExecutionOutcome` 区分 Executed、Failed 与 Unknown。

## 使用与提交

`new(config)` 创建空状态；`prepare(commit_id, command, now_ms, knowledge)` 返回 `Prepared<RecoveryState, RecoveryEffect>`。准备不修改原状态；调用方持久保存新增 `RecoveryEntry` 及提交身份，原子比较聚合版本后调用 `confirm`，再安装结果并处理效果。请求绑定配置、先前完整历史摘要、完整事件与时间。

执行只通过 `AuthorizeExecution`，要求审批域确认消费后返回的 `ExecutionPermit`；流程确认后通过 `RecoveryEffect::Execute` 交付许可。调用方持续保证目标所有权和当前故障条件，`TargetAuthority` 不是分布式锁。观察、验收和核实证据接受最多 30 秒的新鲜度窗口；时间由可信调用方显式输入。工具、执行和总结预算由调用方实施。

## 恢复与核实

`latest_entry()` 访问最新条目，`entries()` 导出完整历史；`restore(config, entries)` 重复实时验证并拒绝身份、顺序、内容或迁移不一致，不返回执行效果。随后必须先提交 `Recover`。审批等待转 `Paused`，显式 `Resume` 保留原操作与原审批关联；中断执行转 Unknown，保留已提交动作及永久版本隔离，不重新许可。中断总结清除调用关联但保留尝试次数和交付身份，旧回调被拒绝。

`ResultChecked` 同时要求独立执行证据和业务验收。审批仍为 Unknown 时先保存核实事实，审批域提交核实后再完成任务；业务健康不能推断原动作执行。历史 Unknown 的隔离在后来验收成功后仍保留。任务登记对同目标未结束流程互斥，重复故障身份沿用原任务。

## 独立经验任务

业务结果提交同时创建稳定 `ExperienceJob` 并保留完整任务审计快照；失败或 Unknown 的实际动作先写入流程本地隔离，不依赖知识模块可用。`pending_experiences()` 返回未交付任务，`BeginExperience` 确认后产生只读总结效果。`ExperienceSummarized` 保存总结及可选候选，候选来源绑定总结 Harness 与 call ID；`ExperienceFailed` 保留重试状态。`ExperienceJob.record()` 从可信任务提取事实并调用 Core `build_experience`，使用与检索相同的稳定条件构造规则，并把实际执行轨迹放入经验 `actions`；完整观察仍留在任务快照，候选不继承业务成功作为执行或验收事实。调用方持久保存经验后提交 `ExperienceDelivered`；重试不重新执行修复。

完整流程见[恢复参考](../../../../../docs/control/recovery.md)，提交与历史义务见[领域维护](../../../../../docs/control/commits.md)，开发规则见[AGENTS](AGENTS.md)。
