# 恢复编排

[workflow.rs](../workflow.rs) 导出 `RecoveryState`、`RecoveryConfig`、`RecoveryCommand`、`RecoveryEvent` 和证据类型。调用方输入已提交事实，Host control 返回待提交状态与后续操作意图，不启动运行循环。

实现位于私有 [service](service/README.md) 模块。完整过程见[恢复流程](../../../../docs/control/recovery.md)，开发规则见[AGENTS](AGENTS.md)。

恢复入口只接收当前配置及当前事件历史，`StartRepair` 是唯一修复主流程；配置使用 `schema_version = 2`，严格拒绝未知字段。
