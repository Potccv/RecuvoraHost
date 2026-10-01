# 远端 Harness 适配规范

- 只使用 本包 protocol 消息和 Host Harness 契约，不复制 wire 类型。
- 保留超时、取消、断连与 Unknown，不自动切换 provider 或重放工具副作用。
- 审批会话必须独立、Hidden 且无工具；回调身份和参数由 Host 校验。
