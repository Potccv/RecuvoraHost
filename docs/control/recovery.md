# 恢复流程

`RecoveryState` 计算单一目标的恢复流程，Host 服务加载和提交状态并执行操作意图。接口见[状态机实现](../../src/control/recovery/workflow/service/README.md)，提交职责见[架构](../architecture.md)。

## 统一 Harness 修复

1. `Register` 登记当前活动故障，重复故障身份不会产生第二次修复，同目标非终态和 Unknown 阻塞新任务。
2. Host 服务提供当前观察并调用 `RecoveryCommand::StartRepair`。Host control 调用 Core 业务函数，一致合并可信 `ProblemContext.conditions` 与 `TargetBinding.required_facts`，注入保留的 `fault_fingerprint`，再结合关键词精确检索 `RepairExperience`；同名条件值冲突或输入占用保留键会被拒绝。当前观察必须满足这些稳定条件，额外 facts 完整保留在请求中作为证据，但不自动成为经验条件。Core 计算全部匹配项后，按稳定顺序选择最多四条能完整放入请求的条目。完整请求 JSON 最多 32 KiB（`MAX_REPAIR_REQUEST_BYTES`）；超预算经验整条省略，后续较小条目仍可入选，不截断内部动作或证据。`matched_experience_count` 是预算筛选前的完整命中数，`experiences` 是实际附带项，因此空列表仍能区分未命中与命中项全部超预算。失败与 Unknown 经验可作为标明结果的参考；命中不授予执行权限，检索错误不能解释为未命中。
3. 请求包含故障、观察、参考经验、逻辑 Harness、目标限制、工具预算和可信委托，并要求总结经验与评估脚本化。`approval.allowed_action_kinds` 必须显式包含 `repair_with_harness`；Host control 提交完整请求后返回 `RequestApproval`。
4. Host 服务在审批域保存原操作申请，再以 `ApprovalAttached` 绑定原审批。当前政策、审核身份与期限仍由审批域校验；Host 服务持有目标所有权和当前故障复核边界，消费一次许可后将其移入 `AuthorizeExecution`。流程提交确认后返回 `Execute`。
5. Harness 内部工具调用由Host 服务管理。会话最多允许一次具体变更；Host 服务在发送前提交 `RepairActionPrepared { action }`，其中 `RepairArtifact` 的种类必须属于 `target.allowed_action_kinds`，内容及前提受校验，来源绑定当前 Harness 和原操作。`RepairReceipt.execution_trace` 必须与已提交动作精确一致，也可以没有动作。
6. Host 服务先提交审批执行结果，再提交匹配的 `ExecutionRecorded`。`RepairExecutionOutcome::Executed` 产生 `Verify`，独立业务验收成功且执行者确认停止才得到 `Completed`。明确失败结束为 `Failed`；无法确定执行或业务结果时进入 Unknown。
7. 业务结果提交同时建立稳定 `ExperienceJob`，实际动作的失败或 Unknown 隔离也在该提交中保存。业务完成不等待总结，恢复不重新派发动作。

不含参考经验的必需请求内容已经超过预算时，准备返回结构化错误，原任务不变；Host 服务须提供有界的故障和观察输入。

配置只接受 `schema_version = 2`。`TargetBinding` 保存逻辑目标、执行者、动作范围、业务验收配置、必需事实和动作超时。Host control 不解释动作 payload 的语言、平台或具体提供方；Host 服务实施调用预算、超时、能力路由及实际动作。

## 独立总结与经验交付

Host 服务提交 `BeginExperience` 后才取得 `SummarizeExperience`，使用独立只读 Harness 会话生成 `ExperienceReport`，回传 `ExperienceSummarized` 或 `ExperienceFailed`。回调绑定调用身份，候选生成来源绑定总结 Harness 和 call ID；重启使旧回调失效。总结失败保留同一经验身份，重试增加尝试次数，不改变业务结果或重新执行修复。

`ExperienceReport` 包含总结、经验教训、相关输入经验 ID 和 `Scriptability`。引用只能来自本次请求的经验列表。`Possible` 允许附带 `RepairArtifact` 候选；`NotSuitable` 和 `Undetermined` 必须说明原因且不带候选。候选只保留不可变内容，不继承本次业务验收、不产生许可；后续 Harness 仍需按当前事实判断适用性。

Host 服务从 `pending_experiences()` 读取保留完整任务审计快照的未交付任务，完成总结后调用 `ExperienceJob.record()`。生成的经验条件使用与检索相同的稳定合并和故障指纹注入规则，不吸收观察中的额外动态 facts；`RepairExperience.actions` 来自已提交执行轨迹。Host 服务经知识域 `TrustedRepairExperience::attest` 和 `RecordExperience` 可靠保存经验后，再提交 `ExperienceDelivered`。相同经验身份只接受完全相同内容；成功、失败和 Unknown 分别保存，后来的成功不撤销早期隔离。

Host control 记录总结尝试和交付状态；恢复服务负责调度调用和可靠保存。知识容量或保存错误不改变任务执行结果，也不得通过删除失败经验、动作版本或更换幂等身份绕过。查询和容量细节见[知识接口](../../src/control/recovery/knowledge/README.md)。

## 审批关联恢复

完整 Harness 请求、观察与稳定操作先于审批申请保存。Host 服务重试原操作申请时不得更换身份或续期；审批域按完整操作与政策幂等。关联提交中断后，流程进入 Paused，显式 `Resume` 在关联缺失时返回原操作申请意图。已有关联则保留原审批和期限，不能创建新的执行机会。

已消费或已终结审批在流程回执缺失时，通过 `ApprovalAttached` 或 `ApprovalResolved` 进入 Unknown 并保留原操作。审批拒绝、过期、取消或撤销终止当前申请，不产生替代修复流程。

## 重启与取消

`restore` 只验证当前协议的完整历史并重建状态，随后必须提交 `Recover` 才接受其他变更。等待审批转 Paused，需要当前任务版本显式 Resume；中断执行转 Unknown，保存已提交动作及永久隔离。中断总结保留已消耗尝试次数并清除调用关联，迟到回调被拒绝。审批域另行通过 `prepare_recovery` 提交恢复事实，不重新发放已消费许可。

Host 服务在恢复、取消或重试前负责协调仍运行的外部调用。取消和网络断连不证明原执行者停止。未派发任务取消前须先取消已关联审批；已有操作但关联尚未解决时必须先核实原审批；已派发任务只能通过独立证据核实结果。

## 结果核实

`ResultChecked` 分别接受绑定原操作的 `ExecutionResultCheck` 与 `BusinessVerification`。业务健康不能证明动作执行；执行者、操作、目标、验收配置、停止状态和证据新鲜度均需匹配。已确认的执行事实不能被后续相反结果覆盖。

审批仍为 Unknown 时，先把独立证据提交到流程，任务保持 Unknown；Host 服务再按这些证据提交审批域 `Reconcile`，最后以当前任务版本、相同证据和更新审批事实再次提交 `ResultChecked`。任一步中断均保留原事实、许可消费和隔离，不重放动作。

已确认未执行进入 Canceled，原许可保持已消费。已确认执行但业务验收未知继续 Unknown，可提供新的只读验收证据；后续成功仍保留历史 Unknown 的动作版本隔离。跨域顺序和维护约束见[领域维护](commits.md)。
