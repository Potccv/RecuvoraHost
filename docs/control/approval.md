# 审批与一次性执行许可

审批域以 `ApprovalLedger` 计算纯领域迁移。Host 服务保存并确认审批提案，负责实际审核调用、身份认证与可靠提交；Host control 保留硬政策、审核身份、期限、一次消费和 Unknown 约束。详细 API 由[审批模块](../../src/control/recovery/approval/README.md)维护，跨域顺序见[领域维护](commits.md#跨域提交顺序)。

## 决定与身份

人工与指定 Harness 的审核均受当前硬政策限制。Host 服务认证调用者并限制 API 访问，`actor`、Harness 和会话身份用于归属与审计，不是认证凭据。模型提供评估内容，Host 服务绑定可信审核来源和具体尝试；迟到结果不能替换当前审核。Harness 结果必须使用已提交 `BeginReview` 返回的尝试调用 `AssessAttempt`；协议不提供无审核尝试的评估入口。

`prepare_request` 保留完整 `ProposedOperation` 和 `ApprovalPolicy`，`find_operation` 用原任务及操作身份查找已有请求。Host 服务必须先提交恢复任务的操作意图，再创建或关联审批；审批记录存在不代表恢复域已经确认关联。

`BeginReview` 的确认结果提供审核副作用，人工决定通过带记录版本校验的 `HumanDecision` 提交。审批通过本身不执行任何动作；知识复用资格、故障确认、UI 状态或模型回答也不能代替许可。

## 从批准到执行

`Consume` 校验当前政策、批准状态、完整操作和有效期，同一目标存在 Executing 或 Unknown 时拒绝新的消费。Host 服务原子提交并确认后得到 `ApprovalEffect::Execute(ExecutionPermit)`；该许可不可复制、不可反序列化，也不会由历史恢复再次生成。

在恢复流程中，Host 服务把许可移入 `RecoveryCommand::AuthorizeExecution`，同时提供当前审批记录、目标观察、故障事实和所有权代次。Host control 再次检查操作绑定、故障、环境、动作范围及隔离状态；只有恢复提案也可靠提交并确认后，才产生 `RecoveryEffect::Execute`。

Host 服务必须在审批消费、恢复授权和派发之间持续保护目标所有权与当前故障版本。实际执行由Host 服务对接的能力模块完成；结果通过保留的原许可交给 `ApprovalLedger::prepare_complete`，先提交审批结果，再提交恢复域 `ExecutionRecorded`。不可用序列化审批记录替代许可构造直接执行。

## 中断与结果核实

恢复审批历史不会自动开始审核或执行；`prepare_recovery` 的确认处理恢复边界，恢复编排另需提交 `Recover`，暂停的原任务通过 `Resume` 显式继续。取消、撤销或中断的执行可能进入 Unknown，取消请求不证明执行者停止。

Unknown 要求Host 服务获取绑定原操作、目标和执行者的独立执行证据，并分别提供业务验收。恢复域先通过 `ResultChecked` 保存并校验这些证据；审批仍 Unknown 时不最终完成任务。Host 服务随后据此提交审批 `Reconcile`，再以相同证据提交恢复域的最终结果核实，不生成新许可，也不根据业务健康推断原操作是否执行。

已知执行事实不能被相反结论覆盖。失败或 Unknown 的经验隔离不因后来的成功核实自动解除。完整流程和允许的后续动作见[恢复流程](recovery.md)，持久恢复与目标释放条件见[领域维护](commits.md)。
