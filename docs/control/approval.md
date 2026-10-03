# 审批接入

Host 使用 Core `ApprovalPolicy` 和 `ApprovalRecord`，通过认证入口注入人工决定。actor 只来自可信配置身份；客户端或模型不得构造提交回执或执行许可。

自动恢复由 Core 发起审核调用；Host 将 ReviewInput 路由到独立 Harness 审核会话并提供可信 ReviewerIdentity。审核角色不具备修复工具。人工决定与审核建议由 Core 校验，Host 保存聚合提案后才返回确认。

Host 只接受已确认的 ExecutionPermit 构造内部 AuthorizedRepair，并保持当前故障和目标所有权保护到实际发送。副作用发送失败、断连或取消不能降为可以安全重试。

独立文本修复仍使用 ApprovalStore 保存 Core 审批域，遵循本机文件边界，不与自动恢复共享任务日志。使用方法见[审批](../approval.md)，持久边界见[提交与历史](commits.md)。
