# 恢复流程与远端后端

本目录实现 Host 的持久提交、调用监督与外部端口；恢复阶段、完整操作绑定、审批消费条件和知识隔离规则由 Core 0.2 的公开领域接口计算，不在 Host 重写状态机。

`service.rs` 保留 `RecoveryService` 管理接口。恢复事件交给 `RecoveryState::prepare`，Host 先原子核对版本并同步完整提案日志，再确认并处理效果。审批及经验适配见 [persistence](../../persistence/README.md)。恢复日志 `recovery.jsonl` 的固定上限为 256 MiB，独立派发边界日志 `dispatch.jsonl` 为 64 MiB；旧格式不会作为空存储打开。日志损坏、提交结果不确定或文件身份变化均阻止继续派发与释放目标所有权。

`storage_layout.rs` 在打开日志前持有状态目录的 `recovery.lock` 独占锁，并持续校验文件身份。旧迁移入口标记和代次子目录明确拒绝，不解析、安装或切换旧代次。

恢复流程先保存原操作，再幂等关联审批。最终检查当前故障、目标所有权和知识资格后，持久消费审批许可；第二次持久确认恢复授权后才允许后端执行。`dispatch.jsonl` 以完整操作记录准备和派发意图。许可消费后授权失败先转 Unknown；存在可靠的未派发边界时，先保存独立 NotExecuted 证据，再核实审批并提交最终流程结果。存在派发意图的中断不自动重发。`check_result` 对 Unknown 先保存独立执行及业务证据，再核实审批，最后使用同一证据提交最终结果；健康本身不证明执行过。重启会续接已保存核实证据与审批事实之间的未完成提交；独立节点证据超过 Core 的 30 秒有效期时继续保留 Unknown，须重新读取，不因重启改写原证据时间。Host 持久未派发边界可以重新核实，不伪造业务健康。

`harness_repair.rs` 实现统一修复会话和独立只读总结。新授权类型 `repair_with_harness` 绑定完整故障、环境、经验与委托；`apply_repair` 发送前通过派发门提交具体动作，最多一次变更。业务结果来自执行器与独立验收；经验总结可无脚本，自动尝试有界，失败可显式独立重试。所有恢复统一进入该流程，具体行为见[恢复流程](../../../docs/recovery.md#统一修复与经验总结)。

`executor.rs` 维护脚本平台、允许语言、只读检查名称及节点脚本格式。`RepairBackend::persistence_binding()` 将这些额外 Host 限制绑定到恢复与派发日志头；同一状态目录改变限制会在恢复历史或审批前拒绝打开。嵌入后端须绑定所有影响派发的额外配置，包装后端须转发该值。审核上下文同时包含执行器范围，Core 仍只接收中立动作。

`contract.rs` 定义 Host `RepairBackend`、输入和不可自行构造的 `AuthorizedRepair`。`node_backend.rs` 解析可信的 node/workspace/endpoint，提供 inspect、独立 review、execute、verify 和 summarize。`check_task_result` 从原执行节点分别读取 reconcile 与 verify 证据；HTTP 不能提交这些权威事实。

`incident_guard.rs` 在监控的拥有式异步门中复核故障、版本、覆盖及单调新鲜度。许可消费、恢复授权与真实网络发送之间持续持门，客户端在握手完成、发送执行请求前再次复核；发送完成才释放。同步监控管理在门被占用时返回忙碌，不阻塞运行时线程。低层自定义 `IncidentGuard` 必须实现 `acquire_dispatch`，默认拒绝派发。普通 `with_current` 仅提供同步登记检查。

`ownership.rs` 与 `storage_paths.rs` 维护文件身份、跨实例锁及持久目标归属。所有保护同一规范目标的存储共用稳定 `ownership_dir`；不同状态目录不能绕开非终态或 Unknown。查询无需绑定归属，提交与推进须绑定 `TargetOwnership` 和 `IncidentGuard`。关闭先停止派发、请求取消并等待监督调用，再确认全部日志可靠及任务和审批无剩余执行权威，才释放持久归属。关闭使用先同步全部日志并保留句柄、再释放归属、最后关闭句柄的顺序；同步或归属释放失败时保留全部句柄，允许重试。

经验从 `pending_experiences` 按记录时间与稳定身份顺序交付，先幂等保存 `RepairExperience`，再提交 `ExperienceDelivered`。任务业务终态与经验总结及交付独立；积压不重新执行修复，失败或 Unknown 的动作隔离事实不会因经验存储失败消失。`scheduler.rs` 只负责固定触发、去重、串行推进和取消后等待。配置及认证管理接口见[恢复流程说明](../../../docs/recovery.md)。
