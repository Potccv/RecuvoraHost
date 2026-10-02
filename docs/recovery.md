# 自动恢复流程

自动恢复流程把故障诊断、审批、执行、验收和修复经验记录串联起来。Host 的 `RecoveryService` 提供恢复接口，`RecoveryScheduler` 安排处理顺序；Core 负责最终判定，Host 保存已确认的事件历史。HTTP 任务详情中的 `result_check` 保存执行核实记录，执行证据的 `checked_at_ms` 表示按证据年龄和调用耗时计算的证据时刻。

## 如何启用

在服务配置中填写 `recovery_config`，指向源码外的恢复流程配置文件。同时需要 AI 服务接入（Harness）、扩展和监控配置。完整组合见 [server.recovery.example.json](../profiles/server.recovery.example.json)，流程配置见 [repair.recovery.example.json](../profiles/repair.recovery.example.json)。普通 Host 启动或插件接口登记都不会自动派发修复。

`RecoveryHostConfig` 包含 Host 的存储目录、必填共享 `ownership_dir`、故障触发规则、调度间隔和存储限额。内层 `recovery` 由 `RecoveryConfig` 定义，审批与修复经验配置分别使用 `ApprovalStoreConfig`、`KnowledgeStoreConfig`。Core 只保存逻辑 Harness 和执行器身份；Host 解析实际连接地址（endpoint）与节点工作区。

启动调度前，Host 为规范目标绑定 Host 的 `FileTargetOwnership`。所有保护同一目标的恢复存储必须使用同一稳定所有权目录；不能随状态目录更换权威。关闭或进程退出后，未完成任务与 Unknown 仍保留持久所有者，只有原存储可以恢复；全部终结且无未知执行审批时，Host 才释放所有权。低层打开 `RecoveryService` 的嵌入方须自行绑定 `TargetOwnership` 和 `IncidentGuard`；只打开日志不能提交或推进恢复，新故障登记须经过当前权威故障复核。

## 统一修复与经验总结

新模板在 `recovery.approval.allowed_action_kinds` 中显式允许 `repair_with_harness`。Core 的 `StartRepair` 生成包含故障、当前观察、最多四条相关经验和可信委托的统一请求；已知和未命中经验都进入同一 Harness 修复会话。参考经验不产生权限，知识读取失败不会降级为空经验。旧 `execute_script` 委托仍使用兼容脚本流程，不自动扩大为会话授权。

当前 `NodeRepairBackend` 只提供 `inspect_target` 和 `apply_repair` 两个 Host 工具，不开放供应商原生 shell、文件或网络工具。`apply_repair` 在固定目标与允许语言范围内最多派发一次变更，使用节点既有 `execute_script` 能力；具体脚本先经 Core `RepairActionPrepared` 校验并持久保存，再在实际发送前复核故障、政策、所有权和知识门。第二次变更被拒绝，不因执行失败或断连自动重试。这个动作产物不等于可复用脚本；Core 请求不要求前置脚本方案。

Host 从执行节点取得回执，模型最终文本不能声称执行成功。`execution_trace` 保存实际动作，独立 verify 决定业务结果；Unknown 保留原操作与隔离，reconcile 使用同一 operation_id。执行结束后的模型回答失败不能覆盖已经取得的独立执行回执。

每个结果建立独立 `ExperienceJob`。Host 使用无工具会话请求经验总结、旧经验关联和脚本化判断，允许 `possible`、`not_suitable`、`undetermined`；候选脚本可为空，生成后不会自动成为已验证脚本。总结和经验保存失败不重跑修复，也不改变已确认业务终态。调度器每轮处理最多四个待总结工作，每个工作自动尝试最多三次；可信嵌入方可调用 `retry_experiences` 显式再试一次。重启保留尝试次数，迟到回调失效。

任务详情的 `experience_jobs` 展示未交付工作的次数、总结状态和错误；知识搜索响应的 `experiences` 返回独立经验，旧 `items` 保留原脚本案例。实际接口见[HTTP API](api/http.md)。经验保存采用稳定身份和可靠提交，脚本候选需要后续单独验证，当前不自动晋升为 Host 直接执行方案。

## 故障如何进入流程

恢复流程调度器从监控读取仍未解除的目标故障（Target），按配置的 monitor/rule 绑定生成 ProblemContext。采集不完整故障（Coverage）不能触发修复。重复通知和重启不会重复诊断同一故障；暂停（Paused）和未知执行结果（Unknown）不会自动继续。

兼容脚本流程依次处理排队（queued）、诊断（diagnosing）、等待审批（awaiting_approval）、执行（executing）、验收（verifying）阶段；最终结果与待交付修复经验同时持久提交，经验交付单独确认。

诊断会查找适用的修复经验并形成方案。审核按 human（人工）、harness（AI）或 human_then_harness（先等待人工，到期转交 AI）规则进行。批准后仍会再次核对故障、目标条件与完整操作，先保存即将执行的操作，再使用一次性执行许可调用节点。独立验收决定业务是否恢复；保存修复记录与经验失败时，只重试保存，不重新执行动作。

旧脚本案例搜索仅返回条件完全匹配的已验证记录，并排除已被隔离的脚本版本。Core 会隔离曾失败、结果未知或停用的固定脚本版本，停止将其作为可用经验推荐。即使核实后任务已完成，也不会自动恢复推荐该版本。

## 人工决定与暂停任务

人工决定接口只保存批准、拒绝或转交决定。已经启用的恢复流程调度器在后续轮次继续处理获准任务。重启后的批准任务保持 paused，须携带当前任务版本号（revision）调用 resume，才能继续。

## 节点执行与结果核实

`NodeRepairBackend` 实现 Host RepairBackend 接口。执行 Harness 与审批 Harness 使用独立隐藏会话，审批会话没有工具。执行器节点实现 `recuvora.repair` v1 的 inspect（检查）、execute_script（执行脚本）与 verify（业务验收）。Host 不启动脚本解释器，也不提供实际脚本沙箱。

核实未知执行结果还需要节点声明只读 reconcile 方法，并在 Host 配置中允许调用。Host 先查询原 operation 的执行结果，再单独调用 verify，把执行证据和业务验收证据交给 `RecoveryService::check_result`，由 Core 校验、Host 保存结果。客户端不能提交 outcome、执行者停止状态、证据或 actor。方法不支持、证据过期或身份不一致时核实失败，不重复执行脚本。

节点 reconcile 请求：`{"target_id":"target","operation_id":"operation-id"}`。响应包含 operation_id、target_id、executor_id、outcome（executed/failed/not_executed/unknown）、executor_stopped、evidence_refs 和 age_ms。Host 结合证据年龄和调用耗时计算证据时刻，Core 再检查证据是否过期、任务 revision 和已保存的结果。目标当前健康不能证明该操作曾执行。

## 接口与限制

HTTP 路径、权限和 revision 见 [HTTP API](api/http.md)，审核规则见 [审批](approval.md)。当前没有独立的恢复流程 CLI 管理命令；`serve --config` 根据配置启动服务，Rust 应用也可直接使用 HostRuntime 和 Core 公共接口。

每个 HostRuntime 管理一个固定目标的恢复流程。它不接受任意脚本上传、任意 shell、自动节点安装或配置热更新。执行结果未知时，继续阻止同一目标上的冲突任务。协议测试替身的结果不能代替真实执行节点、进程监督或业务恢复验收。
