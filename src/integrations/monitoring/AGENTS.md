# 监控适配规范

继承 [集成层规范](../AGENTS.md)。实现位置见 [README](README.md)。

- 把外部只读契约转换为 Host ObservationSource/发现来源。
- 不解释供应商私有字段，规则、时效与覆盖由 Host monitoring 维护；故障权威状态由 Core 维护。
- 传输失败、缺失、过期与目标不健康保持可区分。
