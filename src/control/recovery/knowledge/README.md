# 修复经验与动作产物

`recovery::knowledge` 统一保存 `RepairExperience`，提供可信结果登记、不可变动作产物、永久版本隔离与精确检索。调用方负责完整可信历史、原子提交、存储和查询基础设施；领域关系见[恢复领域](../README.md)。

## 经验与产物

`RepairExperience` 保存操作与目标身份、适用条件、关键词、结果、证据、记录时间、实际 `actions` 和 `ExperienceReport`。经验可以没有动作；一次经验最多记录一个实际动作。`ExperienceReport` 保留总结、经验、相关经验 ID 与 `Scriptability`，后者区分 `Possible`、`NotSuitable`、`Undetermined`。`Possible` 可携带候选 `RepairArtifact`；候选不继承业务成功，不证明已执行或已验收。

`RepairArtifact` 使用 `id`、`version`、`kind`、JSON `payload`、`preconditions` 和生成 Harness/会话身份。`validate()` 检查通用身份、正版本、有界类型标签、序列化后最多 32 KiB 的 JSON、条件和来源；Host control 不解释具体脚本语言、平台或提供方格式。条件要求 1..=32 个精确键值。调用方负责解释产物、确认当前执行器支持，并依据当前授权决定执行。

同一 `(id, version)` 在实际动作和总结候选之间共享不可变内容；包括类型、载荷、前提与来源。`KnowledgeState::validate_artifact` 只检查格式及已有版本一致性，不登记、不核实当前条件、不授予许可；隔离需另行调用 `is_quarantined` 检查。单条经验内动作与候选使用相同版本却内容不同，也整体拒绝。

## 可信登记与隔离

调用方从受保护的恢复结果构造 `TrustedRepairExperience::attest`，提交 `KnowledgeCommand::RecordExperience`。断言校验报告、动作数量和内容、身份、精确条件、关键词与非空证据；它不认证调用方、不证明证据真实。断言与命令不提供 `Deserialize`，可反序列化的经验数据不能直接安装为权威状态。模型报告不能独立授予可信结果或操作权限。

相同经验 ID 仅接受完全相同内容，重试不增加经验数量；每次确认的提交仍递增聚合记录版本号。相同 ID 的结果、动作、总结或任意内容变化都拒绝。`Failed` 和 `Unknown` 经验中的实际动作永久隔离其 `(id, version)`；后来成功不会解除隔离，新版本不继承旧版本隔离。仅出现在总结中的候选没有实际执行结论，不因经验结果自动隔离或验证。

恢复流程先提交失败与 Unknown 隔离事实，再交付经验；知识交付不可用时，恢复域仍保留隔离。知识域提交后从完整经验集合重建同样的隔离和不可变版本事实。具体跨域义务见[恢复流程](../workflow/README.md)与[领域维护](../../../../docs/control/commits.md)。

## 查询与导出

`get(id)` 返回完整经验或 `None`。`search_experiences(&KnowledgeQuery)` 要求经验的全部条件与查询精确匹配，查询关键词采用大小写敏感的精确 AND 匹配。条件和关键词各最多 32 项；返回限额为 1..=100。先匹配，后按记录时间降序、经验 ID 升序稳定排序，再应用限额，只克隆限额内的最终结果。查询错误区别于空结果。

公开查询委托 Core `matching_experiences` 计算匹配与排序。恢复编排通过内部 `experiences()` 借用记录集合，交给 Core `prepare_repair` 计算完整命中数和预算内参考，不复制知识快照。引用候选、公开查询结果及其筛选方式均不产生执行权限。

失败和 Unknown 经验仍可检索，作为明确标记的负面参考；任何查询结果都不构成执行权限。检索匹配经验条件，不推断候选动作当前可执行；调用方与恢复流程必须另行检查动作前提、隔离、政策及当前授权。

`snapshot()` 返回 `KnowledgeSnapshot { revision, config, experiences }`，空经验数组也始终输出。快照用于完整查询和传输，不能直接安装为权威状态。`projection()` 返回经验数、唯一产物版本数、隔离版本数及记录容量，不含产物载荷和证据。

## 提案、提交与历史

`KnowledgeState::new(KnowledgeConfig)` 建立空状态；`config()` 与 `revision()` 查询当前配置及聚合记录版本。`propose(commit_id, command)` 返回 `Prepared<KnowledgeState>`，原状态保持不变。调用方原子比较 `expected_revision`、去重提交 ID、保存完整命令和提交请求，确认持久成功后才通过 `CommitReceipt::confirmed` 与 `confirm` 安装新状态。未知提交结果不能视为成功。

提交绑定知识领域、原配置、先前完整历史摘要和本次完整命令。同一提交 ID 不得重复使用；同一经验的幂等重试使用新的提交身份。`KnowledgeState::replay` 从原配置和有序 `KnowledgeReplayEntry { request, command, receipt }` 重放相同领域校验，拒绝配置、历史摘要、版本、命令、回执或身份冲突，不产生任何执行效果。提交约定见[共同操作](../../operation.rs)。

调用方负责提供受保护的完整历史。Host control 无法从删减、重新编号且重新计算摘要的历史证明过去不存在隔离事实；不得删除失败、Unknown、不可变版本或幂等事实后继续恢复。经验快照保存全部事实，但历史重放仍需调用方保存的提交身份和请求。

## 逻辑容量

`KnowledgeConfig` 只有 `max_records`，默认 1,024，有效范围 1..=100,000。它限制经验集合大小，不是物理存储容量。达到上限拒绝新经验，完全相同的幂等重试仍可接受。

`KnowledgeCommand::ExpandCapacity { expected, target }` 要求 `expected` 与当前配置完全相同，`target` 有效且严格增大 `max_records`。扩容不改变经验、动作、候选或隔离事实，仅在提交确认后生效。重放必须从原配置开始，按顺序重放扩容；不提供删除安全事实绕过上限的出口。

## 源码导航

| 文件 | 职责 |
| --- | --- |
| [contract.rs](contract.rs) | 重导出业务类型；定义容量、命令和完整导出 |
| [experience.rs](experience.rs) | 重导出经验与报告；实现可信断言和调用 Core 检索 |
| [state.rs](state.rs) | 提案、确认历史、不可变版本与永久隔离 |
| [validation.rs](validation.rs) | 通用有界数据与条件校验 |
| [query.rs](query.rs) | 精简聚合统计 |
| [mod.rs](mod.rs) | 公开领域入口 |

验证归属见[集中测试](../../../../tests/README.md)，实际检查范围见[实现状态](../../../../docs/status.md)。领域测试不证明调用方持久性、实际执行器或真实业务恢复。
