# 扩展协议 v1：节点与插件接入规范

本文件由 Host 维护，规定语言无关的 UTF-8 JSON 消息、身份、schema 子集、固定限额和网络会话。外部节点/插件自行选择语言并实现接口，不依赖 Host、Core 或任何共享 Rust 协议包。Host 的对应实现位于 `src/protocol/`；Rust 类型只是宿主实现细节，不是外部接入条件。

节点实现顺序：选择下述 WebSocket 或 HTTP 会话映射；实现 hello/ready 身份与契约声明；实现获准业务方法、消息关联、取消和 Unknown；使用[固定 JSON 样例](protocol-v1-vectors.json)验证消息兼容，再验证网络、业务方法及资源收尾。样例中 valid/invalid 仅检查消息形状，不表示声明已注册、调用已获准或顺序合法。每个样例独立，不是可依次发送的完整会话。

协议版本与 Host 软件版本独立。破坏性消息变化提升协议主版本；业务契约单独管理自己的版本。节点可以保存规范样例的本地副本并独立测试，不建立构建时跨项目依赖。

网络、TLS、配置、注册、路由与认证由 Host 和节点分别实现；审批、许可及恢复权威状态由 Core 维护。业务接口入口见[接入指南](extension-protocol.md)，宿主实现接口见[模块 API](protocol-rust-api.md)。本规范不声明已有提供方完成网络迁移。

## 消息表示

业务消息是 UTF-8 JSON 对象，以 `type` 区分消息。下表中的字段除明确说明缺省的列表外均必需。

`u32` / `u64` 表示 JSON 非负整数，范围分别为 0 至 2^32−1、0 至 2^64−1，不是要求使用 Rust。实现应避免浮点解码导致整数精度损失；具体身份、版本及期限还受下述业务规则限制。消息外壳不接受未知字段；自定义业务对象由其 schema 决定。业务方法字段见[节点接口](node-contracts-v1.md)。

| `type` | 字段 |
| --- | --- |
| `hello` | `protocol_version: u32`、`expected_id: string`、`kind: node/plugin` |
| `ready` | 扁平化的 `ExtensionMetadata`：`protocol_version: u32`、`id: string`、`kind`、`contracts: array`、`capabilities: array<string>`、`workspaces: array<string>` |
| `call` | `id: string`、`contract: string`、`version: u32`、`method: string`、`params: JSON value`、`timeout_ms: u64` |
| `result` | `id: string`、`result: JSON value` |
| `error` | `id: string`、`code: string`、`message: string`、`outcome: rejected/unknown/cancelled` |
| `callback` | `id: string`、`parent_id: string`、`method: string`、`params: JSON value` |
| `cancel` | `id: string` |

`ready` 的 `capabilities` 与 `workspaces` 缺省为空列表。每个契约为 `{id, version, methods}`，每个方法为 `{name, read_only, input_schema, output_schema}`。`read_only` 是提供方声明，注册和实际调用仍须通过 Host 的独立校验；声明本身不产生权限或事实。

类型反序列化检查字段结构、枚举表示及未知字段。它不会自动调用 `valid_id` 或 schema 校验，也不会确认版本、握手身份、调用关联、方法权限或集合限额；这些检查由消费方显式完成。`params`、`result` 和 schema 使用 `serde_json::Value`，不在消息表示层解释业务字段。

握手示例：

```json
{"type":"hello","protocol_version":1,"expected_id":"observation-node","kind":"node"}
```

```json
{
  "type": "ready",
  "protocol_version": 1,
  "id": "observation-node",
  "kind": "node",
  "contracts": [
    {
      "id": "com.example.observation",
      "version": 1,
      "methods": [
        {"name":"query","read_only":true,"input_schema":{"type":"object"},"output_schema":{"type":"object"}}
      ]
    }
  ],
  "capabilities": [],
  "workspaces": []
}
```

调用、回调和结果示例：

```json
{"type":"call","id":"call-001","contract":"com.example.observation","version":1,"method":"query","params":{"target_key":"target-001"},"timeout_ms":30000}
{"type":"callback","id":"callback-001","parent_id":"call-001","method":"service.call","params":{"node_id":"observation-node","contract":"com.example.observation","version":1,"method":"query","params":{"target_key":"target-001"}}}
{"type":"result","id":"callback-001","result":{"entries":[]}}
{"type":"result","id":"call-001","result":{"entries":[]}}
{"type":"error","id":"call-001","code":"scope_denied","message":"outside configured scope","outcome":"rejected"}
{"type":"cancel","id":"call-001"}
```

这些行展示不同消息形状，不表示一个调用在返回 result 后还可返回 error。顶层结果关联 `call.id`，回调关联 `parent_id`，回调回复使用 `callback.id`。`service.call` 是 Host 的消费约定；`Message::Callback` 本身不授予节点读取或其他服务权限。

## 标识、版本与结果含义

`valid_id` 接受 1–128 个 ASCII 字节，字符限于字母、数字、`.`、`_`、`-`。同一规则用于协议 ID；命名空间归属、保留域、重复项以及 ID 与配置的绑定仍由 Host 检查。`call_id` 根据进程 ID、时间与进程内原子计数生成调用 ID，不是认证令牌或持久业务 operation ID，也不提供跨主机、跨重启的全局唯一性保证。

协议主版本当前为 1，契约的 `version` 是独立的业务契约版本。Host 精确匹配握手身份、角色与协议版本；每次业务调用的完整声明必须与登记时一致，包括列表顺序。重建注册表才能采用新声明。破坏性共同消息变化必须提升协议主版本，新增业务方法须符合相应契约的兼容要求。

`rejected` 只用于确认请求代表的持久操作尚未开始的拒绝；可能已派发但结果无法确认时使用 `unknown`。`cancelled` 不证明既有副作用已撤销。当前 Host 将派发后的未确认取消、断连、协议破坏或排空超时保留为 Unknown，并保留调用身份，不自动重试。Rust `ExtensionError::Cancelled` 表示派发前取消；它与派发后收到的 wire `outcome: cancelled` 不等价。

线协议没有独立的 `failed` outcome。业务上已确认的执行失败可由相应契约的 result 表示；不得将不确定结果统一解释为可重试失败。Host 协议模块的 `ExtensionError` 是 Rust 错误类型，不是独立 wire 消息；schema/value 校验失败使用其 `Rejected`，网络错误到 Unknown 的映射由 Host 完成。

可信回调的派发前拒绝、派发前取消分别回复 `outcome: rejected`、`outcome: cancelled`；回执丢失或其他不确定副作用回复 `outcome: unknown`。回调已派发后的协议、连接、超限输出和收尾错误不得降为拒绝。Host 保存回调 Unknown 及原调用身份，即使节点随后返回顶层成功、取消确认或普通完成文本，也向调用方返回 Unknown；取消排空期间可信回调的最终 Unknown 同样保留。该事实须由独立执行证据核实，不触发自动重放。

## 严格 schema 子集

schema 必须是对象，含一个字符串 `type`，支持 `object`、`array`、`string`、`integer`、`number`、`boolean`、`null`。不支持联合类型，也不宣称完整 JSON Schema 兼容。

| 关键字 | 校验规则 |
| --- | --- |
| `properties` | 仅用于 object；值为属性名到子 schema 的对象，最多 64 个属性 |
| `required` | 仅用于 object；最多 64 个字符串，均须引用已定义的 properties |
| `additionalProperties` | 仅用于 object；只接受布尔值；false 拒绝未声明属性，缺省或 true 允许额外属性 |
| `items` | 仅用于 array；值为单个子 schema；省略时不逐项施加子 schema |
| `enum` | 1–64 个 JSON 值，业务值须与其中一个 `serde_json::Value` 相等 |
| `maxLength` | 仅用于 string；非负整数，按 Rust `char` 即 Unicode 标量值计数 |
| `maxItems` | 仅用于 array；非负整数，限制元素数量 |
| `minimum` / `maximum` | 仅用于 integer/number；数值边界及受限业务数值必须在 ±2^53 内 |
| `description` | 字符串说明，不参与业务值判断 |

`type` 之外所有关键字均可省略。其他关键字，包括 `$ref`、`oneOf`、`anyOf`、`pattern` 等，明确拒绝。integer 接受 `Value` 中的 i64/u64 整数表示，number 接受 JSON 数值表示。

`validate_schema` 检查 schema；`validate_value` 先检查 schema，再检查业务值。单 schema 的紧凑 JSON 序列化最多 64 KiB。schema 节点从根深度 0 计数，沿 `properties` 的子 schema 或 `items` 每次加 1，深度大于 12 拒绝；这个计数不是整个 schema JSON 的对象/数组层数，`enum` 中的业务字面值不作为子 schema 递归。

业务值的紧凑 JSON 序列化最多 256 KiB。值从根深度 0 计数，对完整 JSON 中每个对象属性值和数组元素递归加 1，深度大于 24 拒绝；允许的额外属性、未声明 items 的数组也受此深度限制。带 minimum/maximum 的数值限制避免用 binary64 比较时把超出精确整数范围的大整数错误放行。schema 只证明载荷结构，不替代业务授权、目标验证或权威记录。

## 固定限额的当前执行归属

| 限额 | 当前执行方 |
| --- | --- |
| 协议版本 1、ID 长度 128 字节及字符规则 | Host 协议模块导出版本常量与 `valid_id`；Host 显式调用并核验 |
| 帧/单业务消息 1 MiB | Host 协议模块导出 `MAX_FRAME_BYTES`；网络消费方在读写及 JSON 解码前后实施限制 |
| schema 64 KiB、节点深度 12、属性/required/enum 最多 64；值 256 KiB、完整 JSON 深度 24；数值边界 ±2^53 | Host 协议模块 `validate_schema` / `validate_value`，当前数值仍为实现内限额 |
| 每扩展最多 32 个契约、每契约 1–32 个方法、能力与工作区各最多 64 项 | Host 登记声明时验证，尚未作为Host 协议模块的声明验证 API 或集中常量导出 |
| 单调用最多 1800 秒、最多 64 次串行回调、Host 顶层调用期间最多 130 条入站消息 | Host 调用监督与路由验证，尚未集中到Host 协议模块；130 不表示双向消息总数 |

因此仅反序列化 `Message` 或构造声明不能证明满足以上全部限制。其他消费方须在相同边界落实对应验证。限额在协议模块、声明验证与调用监督边界分别执行；进一步集中常量仍是可改进项，见[实现状态](implementation-status.md)。

## Host 网络会话映射

以下是当前 Host 消费协议 v1 消息的约定，Host 协议模块不提供网络客户端或服务端。Host 支持 ws/wss/http/https；端点必须实现该映射，不能把任意 REST 或模型 API 地址直接当作协议节点。端点配置、Bearer、TLS 信任、连接设置和节点部署由 Host 或独立节点项目维护，不进入共同 wire 类型。

每次探测或顶层调用创建独立会话。探测在握手后释放，业务会话只允许一个顶层调用，期间可双向回调。未知消息、未知字段、错误关联、重复或并发回调均拒绝。回调 ID 不得冒用顶层 ID；处理回调期间继续读取协议，终态不能绕过尚未完成的可信回调处理。

取消或截止时间到达时发送 cancel，Host 缺省最多等待 10 秒终态；在途可信工具处理器仍须等待其持久结果。不自动重试、重定向、重连或续接，不多路复用顶层调用。释放连接或会话不能证明节点执行者已经停止。

Host 的本地连接、握手、I/O、关闭、取消宽限、轮询、队列与调用容量可在受限范围内配置，不在线协议中协商。Host 协议模块不导出 `ProtocolSettings` 或 `NodeSettings`。Host 缺省有 4 个普通调用、2 个审批调用和 1 个独立监控视图描述调用容量；容量满立即拒绝，不积累无界队列。

### WebSocket

ws/wss 的每条文本消息恰好承载一个完整 `Message` JSON 对象，不能拼接多条业务消息，单消息不超过 `MAX_FRAME_BYTES`；二进制消息拒绝。支持 Ping/Pong 与标准 Close，心跳只说明连接活动，不产生目标健康或执行完成事实。节点先接收 Hello 并返回 Ready，再按调用、结果、错误、回调和取消关联规则交换消息。

### HTTP

http/https 使用单一 URL，所有请求访问该 URL：

| 阶段 | Host 请求 | 节点响应 |
| --- | --- | --- |
| 握手 | POST，JSON 正文为 Hello | 200，正文为一个 Ready；恰好一个 `x-recuvora-session` 响应头，值为合法协议 ID |
| 发送业务消息 | POST，携带 session 头，正文为一个 Call、Result、Error 或 Cancel | 202 或 204，空正文，仅确认消息接收 |
| 接收消息 | GET，携带相同 session 头 | 200，正文为一个 Message；或 204，空正文表示当前无消息 |
| 释放会话 | DELETE，携带相同 session 头 | 200 或 204 |

JSON 正文不超过 `MAX_FRAME_BYTES`。HTTP 成功及 202/204 不表示业务完成；顶层 Result/Error 和回调经后续 GET 接收。session ID 只标识本次会话，不构成授权或可恢复的 Harness 线程。会话取消、读取、释放和失联处理仍由 Host 同一监督任务持有，网络或 HTTP 错误不会自动重放。

GET 缺省携带 `Prefer: wait=30`，节点可在期限内等消息，也可直接返回 204；Host 空响应后退避轮询。节点必须设置有限会话存活期限并清理孤儿会话：Hello 创建会话后响应若丢失，客户端无 session ID，不能 DELETE 释放。会话过期或删除不代替对已派发执行者的停止核验。

## Host 业务消费边界

Host 在可信配置下登记插件命名空间契约，并使节点仅实现已登记且完全匹配的契约；`recuvora.harness` v1 与 `recuvora.repair` v1 由内置消费者处理。声明、配置启用和调用白名单均不产生业务写权限，第三方通用路由只允许获准的非保留只读方法。命名空间归属、只读 service.call 回调、节点工作区映射与远端路径解释均由 Host 维护。

Harness 项目、执行/审批会话与工具回调通过现有 Call/Result/Callback 表达；独立审批会话及正式工具许可属于 Host/Core 业务约束。Host 协议模块不定义这些 params/result 的业务 Rust 类型或提供方实现。

修复节点可提供 inspect、verify、execute_script 和可选只读 reconcile。Host 的显式结果核实从绑定节点取得原操作执行事实，再独立 verify，交由 Core 检查、Host 保存；这是明确触发的只读核实，不是自动对账、断连重试或重放脚本。execute_script 仍只能在 Core 消费一次许可后由可信内部路由派发。具体证据格式与恢复流程由 Host/Core 维护，不加入共同 `Outcome`。

Host 还可消费 `recuvora.monitoring_view.v1` 能力下的只读 `describe_monitoring_view`，校验声明式数据后通过其 HTTP UI catalog/view 展示；描述调用有独立容量且不授予普通节点读取回调权限。配置绑定的目标记录查询同样使用已有只读契约调用。UI 描述、HTTP 路由、日志来源配置和业务字段不属于Host 协议模块，Host 协议模块不运行插件 HTML 或脚本。

## 验证边界

[固定 JSON 样例](protocol-v1-vectors.json)覆盖七种消息、两种角色、三种 outcome、拒绝未知消息和字段等形状规则。`tests/protocol.rs` 校验样例、Ready 缺省列表、ID 边界及部分 schema/value 限额；Host 既有隔离网络夹具验证握手、命名空间、白名单、取消/断连 Unknown、回调与业务消费。节点须使用自己的实现测试样例，不能以引用 Host 类型代替互操作验证。

这些检查不穷尽协议边界。全部字节、schema 深度、声明数量等限额的兼容覆盖仍需完善；回调分类、Unknown 保留及 Harness 声明 schema 的固定回归与实际验证结果见[实现状态](implementation-status.md)。

协议夹具不联系真实提供方，也不证明实际节点认证、执行者停止、脚本沙箱、业务恢复、跨机部署或长期稳定性。上述事实须由实际节点、应用及部署分别核验。
