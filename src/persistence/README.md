# Host 持久化

`approval`、`incidents` 和 `knowledge` 提供 Host 单写者 Store，领域事实与迁移由 Core 0.2 的 `ApprovalLedger`、`IncidentLedger`、`KnowledgeState` 判定。`state()` 只提供不可变领域读取，用于可信服务装配。

`journal` 保存格式 2 的配置头和有序事务。每个事务包含完整提交请求及领域事件或命令；原始配置、逻辑域、预期 revision、提交 ID 与内容必须一致。实例持有文件排他锁，并检查文件身份及大小；路径必须位于源码外，不允许链接和别名绕过。同 ID 的不同内容、旧 revision、配置改变和不完整尾记录均拒绝，不删除或修剪历史。

提案仅在追加与 `sync_all` 成功后确认。写入错误使当前实例不可再写，重启从受保护的历史恢复，审批额外持久提交 Core 的恢复事件，把中断执行保守处理为 Unknown。故障观察与检查点处于同一事务。知识命令重放重建验收归属并保留失败和 Unknown 隔离，查询快照不能直接安装。

Harness 审核必须先经 `begin_harness_review` 持久提交审核尝试，再通过 `assess_attempt` 返回绑定结果；不提供无审核尝试的 `assess` 入口。

跨审批与恢复提交的目标所有权、当前故障版本及知识隔离门控由恢复编排服务保持。文件锁和提交回执不证明远端执行已经停止。旧格式导入必须通过显式维护入口；普通 Store 打开旧文件会拒绝并保留原数据。

`KnowledgeStore::expand_capacity(expected, target)` 通过 Core 的显式命令增加领域容量，拒绝陈旧配置和缩小容量，保留案例幂等键与隔离事实。打开存储始终使用原始 `KnowledgeStoreConfig`，扩容后的容量由完整历史重放得到；不得改写配置头，也不会自动提高日志物理字节上限。

集中测试位于 [tests](../../tests/README.md)。`persistence.rs` 验证版本比较、身份与配置绑定、跨进程写锁、损坏保留和领域重放；`persistence_faults.rs` 在同步成功而确认丢失时验证写者停止及 Unknown 恢复。`recovery_commits.rs` 在跨域提交边界直接退出独立子进程，再打开实际 Host 存储，验证原操作与审批关联、消费后的未派发证据、独立结果核实以及知识交付幂等。本机持久化与替身验证不代表真实节点业务验收。

## 离线旧日志导入

`legacy::import_legacy_journal(source, destination, LegacyDomain)` 为可信维护程序转换完整的旧审批、故障和未压缩知识事件日志。调用方必须先停止旧所有者并独立核实所有在途执行；函数锁住来源（审批另持有原 `approvals.lock`），用原始可信限额重放 Core 0.2 校验，完整转换后才安装到不存在的新文件。源数据保持不变，容量不足、损坏、内容不支持或目标已存在时不切换，也不覆盖目标。该 API 不属于模型工具或 HTTP 接口，不替代完整恢复服务的跨域迁移。

导入保留原操作、故障身份、检查点、审核归属、消费事实、案例幂等键和失败隔离。审批通过 Core `ApprovalImport` 校验完整旧事件历史，合法旧 `Assess` 保留原评估、revision 与期限，不补造 `BeginReview`。导入和历史重放不产生审核或执行许可；新 Store 打开后仍需持久恢复中断状态。缺少完整命令历史的压缩知识检查点继续拒绝，不能由查询快照或健康状态补造权威。

### 完整恢复存储

`legacy::import_legacy_recovery_bundle(root, incident_journal, config, ownership, now_ms)` 接收原恢复目录、原故障日志、`LegacyRecoveryBundleConfig` 中的原可信配置与限额、稳定目标所有权和可信时间。原恢复目录包含 `recovery.jsonl`、`recovery.lock`、`approvals/approvals.jsonl`、`approvals/approvals.lock` 与 `knowledge.jsonl`。该接口是维护 Rust API，没有 HTTP 或模型写入口。

1. 停止原 Host 及相关监控写者，独立核实在途调用与提交结果。无法确认执行结果时保留 Unknown，不能把取消当作未执行。
2. 使用原 `ownership_dir` 构造 `FileTargetOwnership`。导入持有原目录锁、所有来源日志锁及同目标租约，核对来源没有改变。
3. 解析完整 format 1/2 工作流快照历史、审批与未压缩知识/故障事件，将任务迁移、原操作、审批消费、知识案例及故障关联交给 Core 校验。旧 Unknown 与尚未消费的批准冲突时，通过专用迁移封锁记录转为 Unknown，不伪造消费许可。
4. 在原目录内创建独立 `.core02-*` 代次，可靠保存全部新日志、导入证明与原恢复日志备份 `legacy-recovery.jsonl`。最后原子替换根 `recovery.jsonl` 为入口标记。发布前失败保留原有效日志；发布后的入口只引用已完成代次，旧版本拒绝解释该标记。
5. 新 `RecoveryService` 继续使用原恢复根目录和同一所有权目录，不改 `recovery.lock` 身份；监控显式使用返回的 `LegacyRecoveryBundleReport.monitoring_directory`。维护 API 不自动改写启用配置。启动后按正常流程人工恢复暂停任务、独立核实 Unknown、重试有序知识交付。

导入不生成新任务来替代原身份，不重放外部动作，不以缺失派发记录推断未执行。旧 Publishing 按已有证据归类为 Completed、Failed 或 Unknown 并保留有序知识交付；复用脚本失败后即使尚有诊断预算，也不在导入时自动续诊断。损坏、缺域、审批或故障关联冲突、证据不足以及不支持的压缩历史会阻止切换。测试和适用范围见 [HOST-003](../../docs/status.md#host-003)。
