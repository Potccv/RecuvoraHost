# 领域接入参考

本组文档维护 Host 如何保存领域提案、传入事实并执行能力，不复制 Core 领域实现。

- [审批接入](approval.md)：认证身份、审核能力与许可派发。
- [恢复接入](recovery.md)：Core 引擎与 Host 能力。
- [提交与历史](commits.md)：聚合日志、派发边界、重开与关闭。

故障台账由本库 [control](../../src/control/README.md) 维护，服务入口见[Rust API](../api/rust.md)。
