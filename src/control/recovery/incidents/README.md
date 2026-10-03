# 故障决策

`IncidentLedger` 是纯计算的故障聚合，负责观察检查点、故障归并、独立故障轮次和人工关注归属。本域不采集、不执行恢复动作、不打开文件或管理数据库。入口见[恢复领域](../README.md)。

## 提案与恢复

`IncidentLedger::new(IncidentLimits)` 创建空聚合；`prepare_monitor(commit_id, MonitorCommit)` 计算检查点与有序信号批次的整体迁移，返回 `Option<Prepared<IncidentLedger>>`。提案不会修改原聚合。调用方将事件、提交身份和聚合记录版本号置于同一事务，原子比较 `expected_revision`，只有可靠提交后才调用 `confirm` 并安装返回的状态。提交结果不明时不能前移检查点。

提交请求绑定故障域、现有配置、完整事件历史的摘要和新事件；提交身份不能复用。最新观察的完整相同重试返回 `None`，不覆盖人工确认或增加异常样本数；调用方需先确认所用聚合仍是当前状态。相同观察序号但不同内容拒绝，同一监控的首次序号必须为一，后续严格递增。

聚合可序列化为配置与事件历史；`entries` 复制导出完整 `Vec<IncidentEntry>`，`latest_entry` 只读访问最新条目，`restore` 使用与实时提案相同的迁移校验重建状态，拒绝序号断裂、重复历史、身份冲突和非法证据。调用方保存、校验传输完整性并管理存储容量；历史摘要及增量保存约定见[领域维护](../../../../docs/control/commits.md#增量绑定与历史导出)。Host control 不定义日志文件格式和轮转机制。`IncidentLimits` 只限定聚合的故障轮次和监控数。

## 领域行为

`IncidentKind::ErrorLog` 保存 Node 已识别的错误。每条来源日志生成独立稳定收件身份；Active 只表示收到日志，不表示目标当前不健康。同身份同内容重试不增加 revision 或 occurrences，改写原文或证据拒绝；Clear 与 Unknown 不适用于错误收据。原文最多 8192 UTF-8 字节；为容纳 JSON 转义后的完整日志和来源证据，收据证据最多 64 KiB。

低层 Target 与 Coverage 保留故障轮次语义：同一监控、目标、规则和种类的未解除异常归并，Clear 解除当前轮次，随后 Active 创建新轮次；Unknown 不解除故障。Node 错误接收不产生 Target，来源异常单独记录为 Coverage；来源恢复不影响 ErrorLog。

`prepare_acknowledge` 校验故障记录版本号与 Open 状态，只记录可信调用方的关注归属，不解除故障、不批准修复。调用方负责认证，`actor` 只是审计字段。观察时间回拨时保留单调展示时间，序号继续负责顺序；检查点与整批信号在验证失败或容量不足时一起拒绝。

查询使用 `checkpoint`、`get`、`list` 和 `map_records`。这些查询只表示提供给 Core 的聚合，不证明调用方存储仍然具有该版本；写入前必须执行原子并发校验。回归归属见[集中测试](../../../../tests/README.md)。
