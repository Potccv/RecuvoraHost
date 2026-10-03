# Host 监控

Host `monitoring` 负责配置、定时器、只读观察轮询、发现、规则比较、时效、覆盖和通过 Host `IncidentStore` 向 Host control `IncidentLedger` 提交并原子保存检查点与故障信号。它不采集具体产品日志，也不执行修复。

采集器位于 Node/插件，通过 Host 扩展协议接口约定返回结构化 provider observation。失联、过期、部分覆盖和缺失样本均保持 Unknown，不推断健康。日志中的文本始终是数据，不能改变规则或产生执行许可。

监控故障只有在 Host 按明确配置启动 `RecoveryScheduler` 后才会进入 Host 恢复流程。确认故障只记录人工已阅，不解除故障也不授权动作。
