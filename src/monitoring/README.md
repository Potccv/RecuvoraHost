# Node 错误日志接收

本模块通过配置授权的只读轮询接收 Node 已报告的错误日志，校验来源、游标和限额后，将每个新错误与批末检查点原子持久化。Host 不比较业务字段、不累计健康阈值，也不从空批、覆盖或时效推断目标健康。Core 负责检查、诊断、审批、执行及独立验收；启用恢复的入口见[恢复流程](../../docs/recovery.md)。

实际外部来源为 `RegistryObservationSource`，只允许已登记的 Node 提供错误事实。Plugin 可登记契约及提供发现入口。可信嵌入方通过 `ObservationSource` 注入同一契约。网络仅连接已运行的 ws/wss/http/https Node，不启动节点进程。

## 接收契约

`MonitorsConfig` 使用 `schema_version: 2`，最多 256 KiB。`monitors` 保存静态定义，`discoveries` 默认为空；静态数量加发现预留目标数量最多 64。`MonitorDefinition` 包含 `id`、`target_id`、`source_id`、可选 `view_role`、`extension_id`、`contract`、`version`、`method`、对象 `params`、`interval_ms`、`timeout_ms`、`stale_after_ms`、`startup_grace_ms`，不包含 `rule`。旧规则配置和旧样本响应被拒绝。

实例 ID 唯一，业务契约不能占用保留命名空间或调用写方法。周期范围 10 ms–1 小时，期限 1 ms–30 秒，来源时效与启动宽限 10 ms–24 小时。固定 params 最多 16 KiB，不能包含 `target_id`、`source_id`、`cursor`、`generation`；Host 在调用时加入这些字段，首次 cursor/generation 为 null，后续来自已提交检查点。

Node 返回 `ErrorLogBatch`：

```json
{
  "schema_version": 2,
  "target_id": "target-001",
  "source_id": "source-001",
  "generation": "generation-1",
  "cursor": null,
  "next_cursor": "position-1",
  "coverage": "complete",
  "has_more": false,
  "source_error": null,
  "errors": [
    {
      "id": "error-1",
      "sequence": 1,
      "age_ms": 0,
      "fingerprint": "provider-error",
      "message": "Provider reported an error",
      "evidence": {"origin": "node"}
    }
  ]
}
```

每批最多 32 条错误和 256 KiB。每条 `message` 为非空、无 NUL 的文本，最多 8192 UTF-8 字节；`fingerprint` 为最长 128 字节的有效 ID；`evidence` 为最多 4096 字节、嵌套不超过 24 层的 JSON 对象。sequence 为正整数，sequence/age_ms 不超过 2^53。age_ms 保留 Node 报告的原始年龄，不作为 Host 健康或执行前提，也不要求 Node 不断重发旧错误。

cursor 必须回显请求游标，next_cursor 非空且最多 4096 字节。代次内序号递增；批内倒序、同序号不同身份、同身份内容冲突均整体拒绝，不推进游标。稳定错误身份由 monitor ID、配置 Node ID、source ID、generation、日志 ID 共同组成，fingerprint 相同的不同日志仍分别接收。

相同身份与不可变内容的重发不改变收据 revision/occurrences；只有 age_ms 可变化。去重从全部已持久错误重建，不受最近 32 个 ID 缓存窗口限制。generation 改变的首批标记部分覆盖，旧收据保留；已退休代次不能重放。每监控最多保存 16 个退休代次，耗尽后明确拒绝新代次，需登记新的监控身份，不静默遗忘代次。

Rust 仍提供 `ObservationBatch = ErrorLogBatch`、`ObservationSample = NodeErrorLog` 类型别名，网络不兼容旧 `samples/value/error` 字段。调度端口名称 `ObservationSource`、`ObservationRequest`、`ObservationFuture` 保留。

## 收据与来源状态

每个新错误生成独立 `IncidentKind::ErrorLog`，rule_id 是完整错误身份的 SHA-256 标识，summary 保留原 message，evidence 保留 Node、来源、代次及完整 log。`condition=Active` 仅表示已持久接收，不表示已验证目标不健康。人工确认只增加确认记录，不解除或修改错误。空批、来源失败、部分覆盖、断连、过期或目标发现缺失均不清除收据。

来源状态单独表达 `Freshness`（missing/fresh/stale）与 `Coverage`（unknown/complete/partial/unavailable），只描述数据接收和连续性。任何合法批次，包括空批，刷新本实例来源接收时间；时效由 Host 单调时钟衡量，重启清空运行期来源新鲜度。has_more、source_error、部分覆盖及代次变化产生 Coverage 问题；完整批次可解除 Coverage，不解除 ErrorLog。

快照保留路由、view_role、running、generation/cursor、last_received_at_ms、来源时效/覆盖、诊断 last_error，并提供累计 `received_error_count` 和完整 `last_error_log`。快照不包含 health、连续成功/失败计数或目标健康结论。view_role 仅用于展示，不参与来源身份绑定或授权。

错误信号与检查点由同一 IncidentStore 提交；写失败停止接收并暴露错误。未改变游标或故障的重复/空批只发布诊断视图，不追加日志。`MonitorHandle::error_notifications()` 返回共享 Notify，成功持久提交新错误后唤醒消费者。通知可合并；消费者须先扫描持久收据再等待，通知不是权威队列。

`repair_incident`、`with_repair_incident` 与 `acquire_repair_incident` 校验收据身份、目标、最低 revision、当前可信配置绑定与运行实例，不要求 fresh、complete 或 unhealthy。它们不授予执行权限；Core 必须自行检查与验收目标。同步门内回调仅允许有界授权持久提交，不重入监控或执行外部动作。Owned lease 覆盖授权到最终发送区间，登记、提交及正常关闭不能跨过；发送后立即释放，不等待 Node 执行结束。

## 发现与生命周期

[discovery](discovery/README.md) 维护最多 8 个授权来源。每项 `MonitorDiscovery` 指定可信路由、轮询限额、max_targets、parameter 和完整 template。发现响应保留独立 schema v1，仅含 complete、targets key 列表及 error，不允许 Node 决定 Host 路由或任意参数。每批最多 64 个唯一 key，限定 ASCII 字母、数字、下划线、连字符，长度 1–64。

生成的 monitor/target/source ID 为模板前缀加点和 key；固定 params 加入 parameter 对应的 key。模板不能预先占用该参数或保留字段。`discovery.` 监控 ID 前缀保留给发现检查点。来源绑定覆盖路由、参数和观察模板；改变绑定必须使用新发现 ID。

新身份先持久登记再派发，max_targets 是该来源终生累计上限。有效部分清单只能增加或确认在场；只有无错误完整清单可确认缺失。非法、重复或超限清单整体拒绝；传输失败不等于删除。删除不释放身份、不清空游标、不解除收据，重现复用原 worker。重启恢复身份后须重新发现确认，宽限内不因等待发现制造覆盖故障。

每监控仅一个在途轮询，当前调用结束后再等待周期，不补跑积压。独立定时器在调用期间检查来源时效。超时/关闭请求取消后仍等待原来源 future 结束；取消、断连或丢弃 future 不证明 Node 停止。发现成员代次与提交门拒绝删除后重现的迟到响应。关闭先拒绝新登记与派发，再取消并排空发现/轮询，最后同步并释放存储。

## 源码职责

| 文件 | 职责 |
| --- | --- |
| [config.rs](config.rs) | schema v2 配置、容量与来源绑定 |
| [observation.rs](observation.rs) | Node 错误批次和调度端口 |
| [snapshot.rs](snapshot.rs) | 来源状态与接收诊断 |
| [shared.rs](shared.rs) | 登记门、失败关闭、通知及任务所有权 |
| [handle.rs](handle.rs) | 查询、收据复查、确认和关闭 |
| [engine.rs](engine.rs) | 实例恢复与初始化 |
| [scheduler.rs](scheduler.rs) | 单在途轮询、来源时效与取消排空 |
| [state/](state/README.md) | 整批校验、持久去重与原子提交 |
| [discovery/](discovery/README.md) | 可信模板、目标登记与在场代次 |
| [support.rs](support.rs) | 限额、诊断文本和时钟单位 |

集中验证见 [monitors](../../tests/monitors.rs) 与 [monitor_wire](../../tests/monitor_wire.rs)。这些验证使用隔离来源和网络替身，不代表实际 Node 业务恢复验收。
