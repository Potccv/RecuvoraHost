# 错误来源适配规范

继承[集成规范](../AGENTS.md)，职责见[README](README.md)。

- poll 只允许已登记的 Node 作为错误日志来源；Plugin 可提供契约登记与发现入口。
- 转换只读契约为 Host ObservationSource，使用严格 schema v2，不把旧健康样本降级为空错误。
- 不解释供应商字段、累计规则阈值或判断目标健康。来源失败与业务错误分别报告，不将断线转换为空的完整批次。
