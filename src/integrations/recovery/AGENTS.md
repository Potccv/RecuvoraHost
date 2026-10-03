# 恢复集成规范

继承 [集成层规范](../AGENTS.md)。实现位置见 [README](README.md)。

- 本目录只实现 Node/Harness RepairBackend、执行器配置和总结输入适配；服务、Core Platform、调度、故障保护、目标归属和聚合提交在[恢复应用模块](../../recovery/README.md)。
- 后端额外派发限制通过 persistence_binding 固定到存储根；包装后端必须转发绑定，不能在重启时扩大旧批准的范围。
- execute 只能接收审批许可及恢复授权均可靠提交后交付的 AuthorizedRepair；实际网络发送前保持并复核故障、知识和所有权门，不能以首次轮询 future 代替派发保护。
- 具体动作使用恢复专用 RepairActionGuard 提交后才交给通用网络发送门；通用 DispatchGuard 不承担领域动作准备。
- Unknown、执行者停止证据和业务验证分别返回应用服务，不根据模型文本制造成功事实或扩大授权范围。
- 核实 Unknown 从绑定执行节点获取独立执行事实和 verify 证据，或使用可靠持久的 Host 未派发边界；不接受 HTTP 正文伪造证据，不推断健康结果。

- `repair_with_harness` 必须显式委托，经验仅作参考。会话内具体动作须先通过 Core 聚合提交，再经过真实发送门；只允许一次变更，结果不确定不得重放。
- 总结会话无工具，经验和可选脚本候选不能自证业务成功。自动总结次数有界，失败保留独立重试状态，不改写业务终态。
