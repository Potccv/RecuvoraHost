# 修复后端适配规范

继承 [集成层规范](../AGENTS.md)。实现位置见 [README](README.md)。

- 实现 Core RepairBackend 与 IncidentGuard 端口，scheduler 只调度 Core，不复制审批或恢复状态机。
- execute_script 只能接收 Core 在持久化执行意图并消费一次许可后交付的授权对象。
- Unknown、执行者停止证据、业务验证和知识交付继续分开记录。
- 核实 Unknown 结果时从绑定执行节点获取独立执行事实和 verify 证据，校验身份与时效后交给 Core；不接受 HTTP 正文伪造证据。
