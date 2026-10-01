# 节点业务接口 v1

本文件补充[扩展协议 v1](extension-protocol-v1.md)的业务载荷。下列对象放入 `call.params` 和 `result.result`，不另建消息类型。节点按 JSON 字段独立实现，不需要 Rust 或 Host/Core 依赖。方法须在 Ready 中声明输入/输出 schema，并由可信 Host 配置加入白名单；仅声明方法不会授予权限。

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

工具处理器的拒绝、派发前取消和 Unknown 按[协议结果分类](extension-protocol-v1.md#标识版本与结果含义)返回；有副作用回调的 Unknown 不能被本次会话的普通顶层结果消除，回调输出超限也保持 Unknown。

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

`operation` 的完整结构如下；字段值为形状示例，不构成任何执行授权：

```json
{
  "task_id": "task-1",
  "task_revision": 1,
  "operation_id": "operation-1",
  "target": "target-1",
  "action": {
    "kind": "execute_script",
    "executor_id": "executor-node",
    "script": {
      "id": "script-1",
      "version": 1,
      "language": "python",
      "platform": "linux",
      "source": "print('example')",
      "preconditions": {"workload_version": "1"},
      "generated_by_harness": "assistant",
      "generated_in_session": "session-1"
    },
    "verification_profile": "workload-health",
    "required_facts": {"workload_version": "1"},
    "timeout_secs": 30,
    "incident_id": "incident-1",
    "incident_revision": 1
  }
}
```

脚本文本最多 32 KiB。节点必须验证目标、执行器、语言、平台、前置条件和自身执行范围，完整保留 operation 身份及不可变脚本版本；Host/Core 不提供节点进程沙箱。执行回执 summary 最多 8192 字节。无效或丢失回执保持 Unknown；同一操作不能因调用重试重新执行。结果核实从原节点查询原操作记录，再独立 verify，不重放脚本；`check_result` 是 Host 管理 API 名称，节点 wire 方法是 `reconcile`。

## 自定义插件与只读观察

插件声明自己拥有的命名空间及版本化方法，Host 配置决定调用哪个方法和参数。普通插件路由仅开放获准只读方法，不能占用 `recuvora` 保留命名空间。业务 schema 应明确字段、类型和限额，并与实际返回值一致。

配置为监控来源的方法返回如下 ObservationBatch；其方法名和业务 value 字段由插件契约决定：

```json
{
  "schema_version": 1,
  "target_id": "target-1",
  "source_id": "source-1",
  "generation": "generation-1",
  "cursor": null,
  "next_cursor": "cursor-1",
  "coverage": "complete",
  "has_more": false,
  "error": null,
  "samples": [{"id":"sample-1","sequence":1,"age_ms":0,"value":{"healthy":true},"evidence":{"record_id":"record-1"}}]
}
```

cursor 回显请求游标，初次为 null；generation 表示来源代次，sequence 在代次内单调递增。coverage 只接受 `complete/partial`；空完整批次不代表目标健康，部分覆盖、失联与过期均不能推出恢复。Host 按配置规则读取 value，并独立验证身份、顺序、时效与覆盖。详细配置见[监控说明](monitoring.md)和[监控模块](../src/monitoring/README.md)。

获准插件可发送 `method: "service.call"` 的 callback，params 为 `node_id`、`contract`、`version`、`method`、`params`。Host 只访问插件 `allow_nodes` 中的节点及明确获准的只读方法；回复使用相同 callback ID。该入口不能调用修复执行方法，也不能隐式授权其他业务。
