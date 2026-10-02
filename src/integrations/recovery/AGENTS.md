# 恢复集成规范

继承 [集成层规范](../AGENTS.md)。实现位置见 [README](README.md)。

- Host 实现 RepairBackend、IncidentGuard、TargetOwnership 与持久提交；领域状态迁移仅通过 Core 公开 RecoveryState、ApprovalLedger、KnowledgeState 等接口。
- 后端额外派发限制通过 persistence_binding 固定到存储根；包装后端必须转发绑定，不能在重启时扩大旧批准的范围。
- execute 只能接收审批许可及恢复授权均可靠提交后交付的 AuthorizedRepair；实际网络发送前保持并复核故障、知识和所有权门，不能以首次轮询 future 代替派发保护。
- Unknown、未派发事实、执行者停止证据、业务验证和知识交付分别记录。跨域中断保留原操作和一次消费关联。
- 核实 Unknown 从绑定执行节点获取独立执行事实和 verify 证据，或使用可靠持久的 Host 未派发边界；不接受 HTTP 正文伪造证据，不推断健康结果。
- 经验按领域返回的稳定顺序交付，可靠提交后再确认交付。关闭须验证全部参与日志的可靠性后判断是否释放持久所有权。
- 保持恢复根目录与 recovery.lock 文件身份，拒绝旧迁移入口和代次子目录；不得以新目录、重建锁或释放 Unknown 归属绕过互斥。

- `repair_with_harness` 必须显式委托，经验仅作参考。会话内具体动作须先通过 Core 提交，再经过真实发送门；只允许一次变更，结果不确定不得重放。
- 总结会话无工具，经验和可选脚本候选不能自证业务成功。自动总结次数有界，失败保留独立重试状态，不改写业务终态。
