# Node 错误日志接收

Node 负责采集和识别业务错误，按[错误日志批次契约](extensions/nodes.md#错误日志批次-v2)返回结构化错误记录。Host `monitoring` 保留配置、只读轮询、发现、来源时效与覆盖监督；它不从原始日志、级别或健康阈值判断错误。

Host 将每条日志的原文、来源身份及证据作为不可变 ErrorLog 收件，与来源游标原子保存。同身份相同内容幂等，同身份改写拒绝；不同记录独立保留，不合并为一个永不结束的监控故障。收件后唤醒 Core 递交调度；Core 的目标互斥或容量限制不会使已接收记录丢失。错误报告不是当前健康事实，也不产生执行权限。

启用 `recovery_config` 或调用 `HostRuntime::start_recovery` 后，同目标错误直接进入 Core 的报告受理流程，完整原文保存到 ProblemContext.summary，附件保存在 report。未启用时仍可靠接收并供查询，稍后启用可继续递交。流程和执行授权见[恢复说明](recovery.md)。

来源错误记录在 source_error/last_error 与 Coverage 中，不冒充目标错误；空批、部分覆盖、失联、过期和人工确认不清除收件。freshness 仅描述接收来源的新鲜度，received_error_count 与 last_error_log 是接收事实，不输出 Host 计算的 health 或连续健康计数。原始文本只作为数据，不作为指令。

`GET /api/v1/monitors/{id}/logs` 分页读取已持久接收的错误日志，不额外请求 Node，也不推进读取游标或重复递交 Core。配置 schema 2 删除 rule；旧配置和旧 ObservationBatch v1 明确拒绝，不静默转换。模块实现见[接收模块](../src/monitoring/README.md)。
