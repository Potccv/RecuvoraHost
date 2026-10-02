# 审批与执行

Core `ApprovalPolicy` 规定目标、动作种类、用户委托范围、审核人及有效期。human（人工）、harness（AI）、human_then_harness（先等待人工，到期转交 AI）三种审核规则均由 Core 校验。配置登记、模型回复、插件声明、UI 状态和人工确认故障都不会生成执行许可。

Host 保存 Core 校验后的完整 ProposedOperation、ApprovalRecord、审核身份及提交请求。执行与审核使用独立会话，AI 审核会话隐藏且没有工具。只有已保存的批准仍有效、实际操作与批准内容完全一致时，才能使用一次 ExecutionPermit。`AuthorizedRepair` 不能由 JSON 构造。

## 自动恢复流程

`/recovery/tasks/{id}/decision` 中的 revision 是审批记录版本号；decision 使用 Core 的 approve/deny/escalate（批准/拒绝/转交）。actor 来自 Bearer 令牌对应的配置身份 operator。HTTP 返回完整 Core 审批记录，摘要不足以支持审批决定。

决定请求只保存决定，已启用的恢复流程调度器负责继续处理任务。`resume` 使用任务 revision，只能恢复 paused（暂停）任务。执行前 IncidentGuard 再次核对故障仍未解除、采集范围完整、数据未过期，再由 Host 持久提交 Core 的许可消费与执行授权提案。

未知执行结果（Unknown）需要执行证据和独立业务验收证据，由 Host 从绑定节点获取。取消、断连、命令退出码或当前健康状态都不能单独证明执行者已停止或动作结果。核实结果不会重新发放旧许可，也不会重复执行脚本。

## 独立本机文本流程

`repair` CLI 与 `/approvals/{id}` 支持 human/harness 审核。approve 只保存决定，apply 再次核验并使用许可执行文本替换，check_result 只检查当前文件。human_then_harness 需要定时调度，因此独立文本入口拒绝该规则，不降低审核要求。

Windows 动作只允许替换白名单内已有 UTF-8 普通文件的全文：最多 64 个路径，每文件 16 KiB。写入中途失败可能留下部分内容，结果保持 Unknown。content_verified 表示文件内容核验，business_verified 表示业务验收；文件读回一致不能证明业务恢复。

接口见 [HTTP API](api/http.md)、[CLI](cli.md) 和[恢复流程](recovery.md)。
