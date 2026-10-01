# 恢复集成规范

继承 [集成层规范](../AGENTS.md)。实现位置见 [README](README.md)。

- Host 实现 RepairBackend、IncidentGuard、TargetOwnership 与持久提交；领域状态迁移仅通过 Core 公开 RecoveryState、ApprovalLedger、KnowledgeState 等接口。
- execute_script 只能接收审批许可及恢复授权均可靠提交后交付的 AuthorizedScript；实际网络发送前保持并复核故障、知识和所有权门，不能以首次轮询 future 代替派发保护。
- Unknown、未派发事实、执行者停止证据、业务验证和知识交付分别记录。跨域中断保留原操作和一次消费关联。
- 核实 Unknown 从绑定执行节点获取独立执行事实和 verify 证据，或使用可靠持久的 Host 未派发边界；不接受 HTTP 正文伪造证据，不推断健康结果。
- 知识按 created_revision 顺序交付，可靠提交后再确认交付。关闭须验证全部参与日志的可靠性后判断是否释放持久所有权。
- 离线迁移保持原恢复根目录与 recovery.lock 文件身份；完整代次可靠保存后才原子激活入口，旧版本须拒绝继续解释激活后的入口。不得以新目录、重建锁或释放 Unknown 归属实现切换。
