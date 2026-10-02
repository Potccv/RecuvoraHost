# 自动恢复流程

恢复流程将故障经验匹配、受授权的 Harness 修复、独立业务验收和经验总结串联起来。Host 的 `RecoveryService` 提供接口，`RecoveryScheduler` 安排处理顺序；Core 负责领域判定，Host 持久保存已确认历史。执行结果与总结交付分别记录，总结失败不能触发再次修复。

## 如何启用

服务配置显式填写 `recovery_config`，同时提供 Harness、扩展和监控配置。完整组合见 [服务模板](../profiles/server.recovery.example.json)，恢复配置见 [流程模板](../profiles/repair.recovery.example.json)。普通 Host 启动或插件接口登记不会自动派发修复。

`RecoveryHostConfig.schema_version` 和内层 `recovery.schema_version` 都为 2。Host 配置包含 `data_dir`、必填共享 `ownership_dir`、`executor`、故障触发规则、调度间隔和存储限额。`executor` 管理具体脚本平台、语言及只读检查查询；Core 只保存逻辑身份、目标事实和允许的动作种类。详细字段见[配置说明](configuration.md#恢复流程配置)。

启动调度前，Host 为规范目标绑定 `FileTargetOwnership`。保护同一目标的所有恢复存储必须使用同一稳定所有权目录；未完成任务与 Unknown 在关闭或进程退出后仍保留持久所有者，只有原存储可继续恢复。全部终结且无未知执行审批后，Host 才释放所有权。低层 `open_recovery(data_dir, config, executor)` 不启动调度；嵌入方须绑定 `TargetOwnership`、`IncidentGuard` 并负责关闭。

## 统一修复与经验总结

`recovery.approval.allowed_action_kinds` 显式允许 `repair_with_harness`。Core 的 `StartRepair` 生成包含故障、当前观察、最多四条相关经验和可信委托的统一请求；`matched_experience_count` 记录本次查询的全部匹配数，`experiences` 只包含在请求预算内实际附带的经验，因此空列表不能解释为知识状态未知。命中经验与未命中经验使用同一 Harness 会话。知识读取失败不能降级为空经验，参考经验也不能生成权限。

审核按 `human`、`harness` 或 `human_then_harness` 规则进行。批准绑定完整会话操作、当前政策、目标与期限；批准后仍复核当前故障、目标条件与一次许可。已提交的许可消费和操作身份不能在重启后重建成新的派发权。

`NodeRepairBackend` 只提供 `inspect_target` 和 `apply_repair` 两个 Host 工具，不开放提供方原生 shell、文件或网络工具。`apply_repair` 在固定目标与 `executor` 配置范围内最多派发一次变更，调用节点的 `execute_script`。Core 的 `recovery.target.allowed_action_kinds` 限定具体动作，当前 Host 适配器只支持 `execute_script`。

Host 校验脚本后封装为 `RepairArtifact { id, version, kind, payload, preconditions, generated_by_harness, generated_in_session }`。Core 不解析脚本语言或平台；它校验中立产物、可信会话归属、允许种类、精确前提和隔离。具体动作先通过 `RepairActionPrepared` 持久保存，再在实际发送前复核故障、政策、所有权和知识门。第二次变更被拒绝，失败或断连不会自动重试。

Host 从节点取得执行回执，模型最终文本不能声明执行成功。`execution_trace` 保存实际中立动作；独立 `verify` 决定业务结果。Unknown 保留原操作和实际动作隔离，`reconcile` 始终查询同一 operation_id。执行结束后的模型回答失败不能覆盖已取得的独立执行回执。

## 独立经验总结

每个结果建立包含完整任务快照的 `ExperienceJob`。Host 为无工具总结会话构造不超过 64 KiB 的只读投影，只固定保留故障指纹、实际动作完整 payload 与来源身份、可信执行/验收结果及相关经验 ID；不重复发送完整审批请求、恢复任务或历史经验。证据引用、动作前提、故障细节和经验短摘要按完整 prompt 的实际 JSON 编码大小整字段装入；放不下时记录省略字段、原条数和编码大小，不截断或改写字符串，Core 中的完整审计快照不变。

总结请求返回相关经验 ID 和脚本化判断，支持 `possible`、`not_suitable`、`undetermined`。只要投影省略了任何候选判断上下文，Host 就把结果收敛为 `undetermined` 并丢弃模型候选，模型不能依据缺失前提产生正向脚本判断。上下文完整时，可脚本化也可以没有候选；候选经 Host 格式校验后保存为中立产物，不继承业务成功，也不自动成为直接执行方案。`summary_timeout_secs` 独立限制总结调用。

总结和经验保存失败不改变已确认业务终态，也不重跑修复。每轮调度最多处理四个待总结工作，每个工作自动尝试最多三次；可信嵌入方可调用 `retry_experiences` 显式再试。重启保留尝试次数，迟到回调失效。

`RepairExperience` 统一保存可信结果、证据、实际 `actions` 与模型报告。相同 ID 完全相同的内容可幂等重试，内容变化拒绝；实际动作和候选的同一产物版本不可变。Failed 和 Unknown 的实际动作版本永久隔离，后来的成功不会清除隔离。失败与 Unknown 经验仍可检索为负面参考；只出现在总结中的候选不因结果自动获得执行或验收结论。

任务详情的 `experience_jobs` 展示未交付工作的次数、总结状态和错误。知识搜索只返回 `experiences`，按精确经验条件与关键词匹配，再按时间降序、ID 升序排序；没有独立脚本案例集合。实际接口见[HTTP API](api/http.md)。恢复域先持久提交隔离事实，再尝试经验交付，知识存储不可用不能丢失隔离。

## 故障与暂停任务

调度器读取仍未解除的目标故障，按固定 monitor/rule 绑定生成 `ProblemContext`；采集不完整故障不能触发修复。重复通知和重启不重复创建同一故障任务。暂停和 Unknown 不会自动重执行。

人工决定只保存批准、拒绝或转交决定，已启用调度器随后继续处理获准任务。重启后的待执行任务暂停，必须携带当前任务 revision 显式 `resume`；原操作、审批期限和已消耗预算继续有效。

## 节点执行与结果核实

`NodeRepairBackend` 实现 `RepairBackend` 的 inspect、review、execute、verify、summarize。执行与审核使用独立隐藏会话，审核与总结均无工具。节点实现 `recuvora.repair` v1 的 inspect、execute_script、verify；Host 不启动脚本解释器，也不提供实际脚本沙箱。

核实未知结果还需要节点声明只读 `reconcile` 并在 Host 配置中允许调用。请求为 `{"target_id":"target","operation_id":"operation-id"}`；响应包含 operation_id、target_id、executor_id、outcome（executed/failed/not_executed/unknown）、executor_stopped、evidence_refs 和 age_ms。

Host 查询原操作执行结果，再独立 verify，将执行证据与业务验收交给 `RecoveryService::check_result`。HTTP `result_check` 保存核实记录，`checked_at_ms` 根据证据年龄和调用耗时计算；Core 校验时效、任务 revision 和身份。客户端不能提交 outcome、停止状态、证据或 actor。方法不支持、证据过期或身份不符则核实失败，不重做动作；目标健康不能证明原操作曾执行。

## 接口与限制

HTTP 路径、权限和 revision 见 [HTTP API](api/http.md)，审核规则见 [审批](approval.md)。没有独立恢复管理 CLI；`serve --config` 根据配置启动，Rust 嵌入者也可使用 `HostRuntime`。

每个 HostRuntime 管理一个固定目标。恢复入口不接受任意脚本上传、任意 shell、自动节点安装或配置热更新。独立文本修复与模拟服务继续由各自入口提供。Unknown 阻止同目标冲突任务；隔离协议替身不能替代真实节点、进程监督或业务恢复验收。
