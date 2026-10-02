# Host 持久化

`approval`、`incidents` 和 `knowledge` 提供 Host 单写者 Store，领域事实与迁移由 Core 0.2 的 `ApprovalLedger`、`IncidentLedger`、`KnowledgeState` 判定。`state()` 只提供不可变领域读取，用于可信服务装配。

`journal` 保存格式 2 的配置头和有序事务。每个事务包含完整提交请求及领域事件或命令；原始配置、逻辑域、预期 revision、提交 ID 与内容必须一致。实例持有文件排他锁，并检查文件身份及大小；路径必须位于源码外，不允许链接和别名绕过。同 ID 的不同内容、旧 revision、配置改变和不完整尾记录均拒绝，不删除或修剪历史。

提案仅在追加与 `sync_all` 成功后确认。写入错误使当前实例不可再写，重启从受保护的历史恢复，审批额外持久提交 Core 的恢复事件，把中断执行保守处理为 Unknown。故障观察与检查点处于同一事务。知识命令重放重建验收归属并保留失败和 Unknown 隔离，查询快照不能直接安装。

Harness 审核必须先经 `begin_harness_review` 持久提交审核尝试，再通过 `assess_attempt` 返回绑定结果；不提供无审核尝试的 `assess` 入口。

跨审批与恢复提交的目标所有权、当前故障版本及知识隔离门控由恢复编排服务保持。文件锁和提交回执不证明远端执行已经停止。旧格式与退休的存储代次不受支持；打开时拒绝并保留原数据。

`KnowledgeStore::expand_capacity(expected, target)` 通过 Core 的显式命令增加领域容量，拒绝陈旧配置和缩小容量，保留经验幂等键与隔离事实。打开存储始终使用原始 `KnowledgeStoreConfig`，扩容后的容量由完整历史重放得到；不得改写配置头，也不会自动提高日志物理字节上限。

集中测试位于 [tests](../../tests/README.md)。`persistence.rs` 验证版本比较、身份与配置绑定、跨进程写锁、损坏保留和领域重放；`persistence_faults.rs` 在同步成功而确认丢失时验证写者停止及 Unknown 恢复。`recovery_commits.rs` 在跨域提交边界直接退出独立子进程，再打开实际 Host 存储，验证原操作与审批关联、消费后的未派发证据、独立结果核实以及知识交付幂等。本机持久化与替身验证不代表真实节点业务验收。

## 独立修复经验

知识日志支持 `RecordExperience`，保存包含已提交动作、业务结果和总结的 `RepairExperience`。只有可信恢复适配器可据已提交业务结果调用 `record_experience`；恢复日志时通过 `TrustedRepairExperience::attest` 重建权威输入，不向 HTTP 或模型开放该入口。相同身份的相同内容可幂等重试，内容冲突拒绝；候选脚本不因经验结果为成功而取得脚本验收。
