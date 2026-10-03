# 审批决策

本域提供纯计算的 `ApprovalLedger`。调用方保存审批事件历史、原子比较聚合记录版本号，并在提交成功后调用 `Prepared::confirm`。本域不打开文件、访问数据库、启动定时器或调用审核执行端口。入口见[恢复领域](../README.md)，跨域流程见[恢复流程](../../../../docs/control/recovery.md)。

## 接口与提交

`ApprovalLedger::new(ApprovalLimits)` 创建空聚合，`prepare_request` 准备绑定完整 `ProposedOperation` 和 `ApprovalPolicy` 的请求。请求身份由聚合下一记录版本号确定，`find_operation` 可按任务和操作身份查找既有请求；重复身份不能创建第二次审批，内容冲突需要拒绝。`prepare` 接收 `ApprovalEvent`、当前政策及调用方时间，返回待提交状态；旧聚合不变。每次提案使用唯一提交身份，历史拒绝重复提交身份。

`Prepared::request` 绑定审批域、原始事件历史的摘要、配置、待提交事件、当前政策与时间，调用方将完整状态或事件以及请求身份置于同一原子事务，比较 `expected_revision` 后再确认。仅发送数据或加入队列不足以确认提交。提交失败、冲突或结果不明不能确认提案。确认不会写入任何存储，`CommitReceipt` 是可信调用方的提交声明。

`ApprovalLedger` 可序列化为配置与事件历史；`entries` 复制导出完整 `Vec<ApprovalEntry>`，`latest_entry` 只读访问最新条目，`restore` 用相同迁移校验重建聚合，不直接反序列化权威状态。历史绑定及增量保存见[领域维护](../../../../docs/control/commits.md#增量绑定与历史导出)。`ApprovalLimits::max_requests` 是领域输入容量限制，不代表物理存储配额。调用方负责保存提交身份、完整历史与审批政策，不能丢失操作身份和一次消费事实。

## 审核规则

`ApprovalChange` 表达人工决定、审核开始与结果、失败转人工、撤销、取消、过期、许可消费及结果核实。需要授权的迁移必须携带与请求完全相等的当前硬政策；人工批准也不能越过目标或动作范围。`HumanDecision` 的 `expected_revision` 拒绝过时界面决定；人工拒绝是终态。

`BeginReview` 同时校验记录版本号、人工等待截止时间与审核预算，提交确认后产生 `ApprovalEffect::Review`。`AssessAttempt` 绑定审核尝试、记录版本号、Harness 身份和截止时间。协议不提供普通 `Assess` 或历史导入入口；已删除的事件不能反序列化为当前输入。调用方提供审核身份，模型仅提供决定和理由；迟到结果无效，失败转人工后不能再次自动审核。过期及失败是显式事件，调用方负责定时提交这些事件。

## 执行与恢复

`Consume` 校验当前政策、有效期和同目标执行或 Unknown 状态互斥。提交确认后才产生 `ApprovalEffect::Execute(ExecutionPermit)`。许可不可复制或反序列化，只绑定原始操作和当前审批记录版本号；调用方还必须完成当前故障、目标归属和环境复核。

`prepare_complete` 消费原始许可并校验聚合身份、完整操作与记录版本号，完成记录同样需要提交确认。公共 `prepare` 拒绝直接构造 `Complete`，历史恢复允许重放已经提交的完成事实。执行中取消或撤销产生 Unknown，不能证明执行端已经停止；迟到许可不能覆盖该状态。`Reconcile` 只接受可信调用方独立核实后的明确终态，永不产生新执行许可。

`restore` 只恢复记录，不产生审核或执行效果，非空聚合必须先通过 `prepare_recovery` 提交一个无效果的恢复事务。该事务将所有 Executing 转为 Unknown，在途审核转为 NeedsHuman，已经过期的待审批或已批准请求转为 Expired；人工等待截止时间不变。在恢复确认之前拒绝其他迁移，旧审核回调不能在恢复后取得授权。新旧实例的目标互斥和聚合原子提交由调用方保证，本域内存对象不提供跨进程锁。

领域回归见[集中测试](../../../../tests/README.md)。
