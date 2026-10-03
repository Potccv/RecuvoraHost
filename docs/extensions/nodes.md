# 节点业务接口

本文件补充[扩展协议 v1](protocol.md)的业务载荷。下列对象放入 `call.params` 和 `result.result`，不另建消息类型。节点按 JSON 字段独立实现，不需要 Rust 或 Host/Core 依赖。方法须在 Ready 中声明输入/输出 schema，并由可信 Host 配置加入白名单；仅声明方法不会授予权限。

## Harness：`recuvora.harness`，version 1

提供方必须为 `kind: node`。`workspace` 是 `{"node_id":"example-node","workspace_id":"default"}`，工作区 ID 必须在 Ready 的 `workspaces` 中；真实目录由节点配置映射。Host 不下发任意本机路径。

| 方法 | params | result |
| --- | --- | --- |
| `projects` | `workspace` | 项目对象数组，最多 512 项 |
| `create_project` | `workspace`、`name: string`、`idempotency_key: string` | 单个项目对象 |
| `run` | 下表所列会话请求 | 下表所列会话结果 |

项目对象是 `{"id":"project-id","name":"Example","roots":["/srv/workload"]}`；只返回节点侧元数据，不能让 Host 把路径当成本机文件使用。项目 ID 非空、最多 512 字节且无控制字符。`projects` 声明只读，`create_project` 与 `run` 声明非只读；节点需记录副作用和未知结果，不能因重连而重复创建项目或会话。

`run` 请求字段：

| 字段 | JSON 内容 |
| --- | --- |
| `harness_id` | Host 选择的逻辑 Harness ID，回调须原样关联 |
| `workspace` | 上述节点/工作区对象 |
| `prompt` | 非空文本，Host 输入最多 64 KiB |
| `model` | 模型名称字符串，未指定时为 null |
| `visibility` | `hidden` 或 `client` |
| `placement` | `{"type":"none"}` 或 `{"type":"existing","project_id":"project-id"}` |
| `role` | `execution` 或 `approval` |
| `tools` | 工具数组，每项为 `name`、`description` 和 `input_schema` |

`run` 结果字段：`thread_id: string`、`session_id: string`、`project_directory: string`、`visibility: string`、`native_project_id: string|null`、`client_project_grouping: object`、`final_response: string`。线程/会话 ID 非空、最多 512 字节且无控制字符；目录为节点侧绝对路径元数据，最多 4096 字节；最终文本最多 256 KiB。可见性和原生项目必须与请求一致。以上结果对象及项目对象不接受未声明的额外字段。

`client_project_grouping` 为 `{"type":"not_applicable"}`、`{"type":"unverified"}` 或 `{"type":"confirmed","client_project_id":null}`；confirmed 的 ID 也可为字符串。原生项目回显不等于客户端已经归组。

Ready 的能力标识按功能声明：文本调用需要 `text`，Client 会话需要 `client_visibility`，项目方法及项目放置需要 `projects`，审批需要 `approval`，工具需要 `tools`。每次审批使用新的 Hidden、无工具、无项目会话；执行上下文不能作为独立审核上下文。

### Harness 工具回调

节点发送 `callback`，其中 `method: "tool"`、`parent_id` 等于当前顶层调用 ID。params 为：

```json
{"harness_id":"assistant","thread_id":"thread-1","turn_id":"turn-1","call_id":"tool-1","tool":"inspect_target","arguments":{"query":"status"}}
```

`arguments` 必须是对象且不超过 64 KiB。只允许本次请求声明的工具；同次调用固定 thread/turn，最多 64 个不同工具 call_id，不得重复派发。Host 用 callback 的 envelope ID 回复 result，其载荷为 `{"content":"bounded text","success":true}`，content 最多 256 KiB；错误使用协议 Error。节点不能把工具许可解释为原生命令、文件写入或模型供应商其他工具权限。

Host 对 `projects`、`create_project`、`run` 校验声明的读写属性、输入和输出 schema；输入不符在派发前拒绝，结果不符不得作为成功项目或会话返回。`create_project` 和 `run` 已派发后的无效输出保持 Unknown；`projects` 的只读无效输出作为契约校验错误返回。节点可声明比上述通用上限更严格的必填字段、数组项数或字符串长度，Host 同时执行这些限额。

工具处理器的拒绝、派发前取消和 Unknown 按[协议结果分类](protocol.md#标识版本与结果含义)返回；有副作用回调的 Unknown 不能被本次会话的普通顶层结果消除，回调输出超限也保持 Unknown。

## 恢复执行器：`recuvora.repair`，version 1

仅 `kind: node` 可提供该契约。`inspect`、`verify`、可选 `reconcile` 必须为 `read_only: true`；`execute_script` 必须为 false，且只能由 Core 消费一次性执行许可后的 Host 内部路由调用。

| 方法 | params | result |
| --- | --- | --- |
| `inspect` | `target_id: string`、`query: string` | `target_id`、`facts: object<string,string>`、`evidence_refs: array<string>`、`age_ms: u64` |
| `verify` | `target_id`、`profile: string`、`operation_id: string` | `operation_id`、`target_id`、`profile`、`healthy: boolean|null`、`executor_stopped: boolean`、`evidence_refs`、`age_ms` |
| `reconcile` | `target_id`、`operation_id` | `operation_id`、`target_id`、`executor_id`、`outcome`、`executor_stopped`、`evidence_refs`、`age_ms` |
| `execute_script` | `request_id: string`、下述 `operation` 对象 | `operation_id`、`target_id`、`outcome`、`executor_stopped`、`evidence_refs`、`summary: string` |

`inspect.query` 和 `verify.profile` 来自可信配置，节点应映射到自身明确支持的检查，不当作任意 shell 执行。`facts` 最多 32 项，键最多 128 字节、值最多 1024 字节。证据引用为 1–32 个非空字符串，每项最多 1024 字节；观察和验收等证据集合不得重复。证据年龄表示节点组装响应时的相对毫秒数，Host 会加入调用耗时，超过 30 秒拒绝；不比较两台机器的墙上时钟。

`execute_script` 的 outcome 为 `executed/failed/unknown`；`reconcile` 另允许 `not_executed`。这些是业务结果，位于 result 载荷中，与 wire Error 的 `rejected/unknown/cancelled` 不同。执行成功不等于业务健康，verify 必须独立取得证据。不能证明停止时不得填 `executor_stopped: true`。

`operation` 保留 task_id、task_revision、operation_id、target；`action` 包含 kind 为 `execute_script`、executor_id、script、verification_profile、required_facts、timeout_secs 和必填的 repair_authorization。`repair_authorization` 是完整已审批 Harness 会话操作，其 action.kind 为 `repair_with_harness`，action.request 保存故障、观察、相关经验、目标和可信委托；外层与内层 operation_id 必须相同。节点必须保留整个授权对象，不能把具体脚本当作一份独立的新授权。

`operation.action.script` 的形状如下；内容仅为示例，不构成执行授权：

```json
{
  "id": "operation-1-action",
  "version": 1,
  "language": "python",
  "platform": "linux",
  "source": "print('example')",
  "preconditions": {"workload_version": "1"},
  "generated_by_harness": "assistant",
  "generated_in_session": "operation-1"
}
```

这个脚本结构属于 Host 到节点的业务协议。Host 将 language、platform、source 封装进中立 `RepairArtifact.payload`，kind 为 `execute_script`，其余身份、前提和来源由 Core 校验并提交。Core 不解释脚本语言或平台；节点仍须执行自身的格式、执行器能力和沙箱校验。

脚本文本最多 32 KiB；封装后的中立 JSON payload 也须不超过 32 KiB，因此实际允许的文本长度还受 JSON 转义及字段开销限制。节点必须验证目标、执行器、语言、平台、前置条件和自身执行范围，完整保留 operation 身份及不可变脚本版本；Host/Core 不提供节点进程沙箱。执行回执 summary 最多 8192 字节。无效或丢失回执保持 Unknown；同一操作不能因调用重试重新执行。结果核实从原节点查询原操作记录，再独立 verify，不重放脚本；`check_result` 是 Host 管理 API 名称，节点 wire 方法是 `reconcile`。

## 自定义插件与只读观察

插件声明自己拥有的命名空间及版本化方法，Host 配置决定调用哪个方法和参数。普通插件路由仅开放获准只读方法，不能占用 `recuvora` 保留命名空间。业务 schema 应明确字段、类型和限额，并与实际返回值一致。

### 错误日志批次 v2

配置为错误来源的方法必须由 `kind: node` 实现，为获准只读方法。自定义契约仍由可信插件登记，Node 声明完全匹配的契约；插件不直接提供错误事实。新业务载荷使用 schema 2，方法的契约版本须与旧观察接口分开；共同消息协议仍为 v1。固定[错误日志样例](examples/error-logs.json)可供独立节点实现校验。

Host 按配置轮询并传入 `target_id`、`source_id`、`generation` 和 `cursor`；首次 generation/cursor 为 null。Node 只返回自己已识别的实时错误日志，不返回等待 Host 计算健康的任意 value，也不混入普通运行日志。批次如下：

```json
{
  "schema_version": 2,
  "target_id": "target-1",
  "source_id": "source-1",
  "generation": "generation-1",
  "cursor": null,
  "next_cursor": "cursor-1",
  "coverage": "complete",
  "has_more": false,
  "source_error": null,
  "errors": [{"id":"error-1","sequence":1,"age_ms":0,"fingerprint":"workload-condition-v1","message":"Original error log text","evidence":{"record_id":"record-1"}}]
}
```

每批最多 32 条，完整编码不超过 256 KiB。id、generation 和 fingerprint 使用协议 ID 字符规则；sequence 为代次内递增正整数；sequence 与 age_ms 均不得超过 2^53。message 为非空、无 NUL、最多 8192 UTF-8 字节的原始错误文本，超限拒绝而不截断；evidence 为最多 4096 编码字节、深度不超过 24 的对象。age_ms 为 Node 组装响应时记录的相对年龄，只是来源证据，不是执行授权。未知字段拒绝。

cursor 必须准确回显请求，next_cursor 为非空且最多 4096 UTF-8 字节的后续读取位置；Node 不得因为连接结束删除尚未可靠交付的错误。generation 表示来源代次。同一来源、代次和错误 ID 的 sequence、fingerprint、message、evidence 不可变；相同日志的重新读取可更新 age_ms，但不能制造新记录。Host 对收件和读取位置同次持久确认，重复数据不重复提交业务任务，同身份内容冲突整批拒绝。每个接收实例最多保留 16 个退休代次，超限明确拒绝新代次，不能遗忘旧代次后接受重放。

coverage 只接受 `complete/partial`，它只描述错误流采集覆盖。source_error 表示来源采集问题，不是业务错误日志。空完整批次、部分覆盖、失联和来源过期均不证明健康或解除旧错误。Host 不根据 level、event、消息文本或连续阈值二次筛选错误；每条有效日志独立进入持久收件，恢复已启用时立即唤醒递交 Core。Core 收到 ErrorLog 问题报告后不复判日志活跃性，按原始文本与证据进入经验匹配及恢复流程；环境观察、审批和验收继续约束后续执行。详见[错误日志接收](../monitoring.md)。

获准插件可发送 `method: "service.call"` 的 callback，params 为 `node_id`、`contract`、`version`、`method`、`params`。Host 只访问插件 `allow_nodes` 中的节点及明确获准的只读方法；回复使用相同 callback ID。该入口不能调用修复执行方法，也不能隐式授权其他业务。

## Harness 修复会话的动作绑定

Host 可在显式 `repair_with_harness` 委托下，通过 Harness 的 `apply_repair` 工具调用已有 `recuvora.repair` v1 `execute_script` 方法。请求仍包含 `request_id` 与完整 `operation`；动作的 `kind` 为 `execute_script`，必填 `action.repair_authorization` 保存完整已审批会话操作，外层 operation_id 与会话相同。Host 在发送前持久保存具体脚本，最多派发一次；节点仍按 operation_id 幂等并提供独立 reconcile/verify，不从 Harness 文本判断恢复。节点必须在自身目标沙箱内校验请求，未知附加业务字段不应被解释为扩大权限。

总结使用 Harness `run` 方法和无工具会话，由独立 summary_timeout_secs 限时，结果由 Host 解析，不新增节点方法。脚本候选与实际动作都转换为中立产物，分别保存在 RepairExperience.report.scriptability 与 actions。候选不能凭模型评估或本次业务成功被标记为通过验收；失败和 Unknown 实际动作版本永久隔离。
