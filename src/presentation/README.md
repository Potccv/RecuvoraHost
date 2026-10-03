# 展示数据转换

本模块将 Host Harness/独立文本流程结果及其中的 Core 审批事实转换为应用 JSON，供 CLI 与 HTTP 共用。恢复流程详情由 Core 记录生成，Host 输出 result_check 核实记录。展示不调用模型、不执行动作、不验证身份，也不改变正式业务记录。

状态、操作关联、实际错误、Unknown 与 business_verified=false 保持原语义；输出 completed 不能解释为业务恢复。所有外部字符串由 JSON 编码，界面与HTTP展示不另建授权规则。

边界见[架构](../../docs/architecture.md)，规范见 [AGENTS](AGENTS.md)。
