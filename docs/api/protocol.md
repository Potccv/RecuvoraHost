# Host 协议模块 Rust 接口

本文件描述当前 Host `protocol` 模块的导出，仅供宿主实现与测试参考；外部节点按 JSON 规范自行实现。协议主版本为 v1；消息的 JSON 约定见 [外部扩展协议](../extensions/protocol.md)，源码入口见 [mod.rs](../../src/protocol/mod.rs)。本模块提供线协议表示和显式校验函数，不提供传输、扩展注册、调用监督或业务授权。

## 导入与公开边界

Cargo 包名是 `recuvora-host`，Rust crate 名是 `recuvora_host`。宿主接口从 `protocol` 模块导入，外部节点不应依赖此包；`schema` 与 `wire` 是私有模块，不能使用 `recuvora_host::protocol::wire::Message` 之类的路径。

```rust
use recuvora_host::protocol::{
    ContractDeclaration, ExtensionError, ExtensionKind, ExtensionMetadata,
    MAX_FRAME_BYTES, Message, MethodDeclaration, Outcome, PROTOCOL_VERSION,
    call_id, valid_id, validate_schema, validate_value,
};
```

## 常量

| 导出 | 类型 | 当前值 | 使用责任 |
| --- | --- | --- | --- |
| `PROTOCOL_VERSION` | `u32` | `1` | 消费者在握手时精确核对协议版本；反序列化不自动比较该常量。 |
| `MAX_FRAME_BYTES` | `usize` | `1024 * 1024`，即 1 MiB | 传输实现限制单个 JSON 消息的 UTF-8 字节数，在发送及接收边界检查；`Message` 本身不检查帧大小。 |

当前公开 API 没有单独的 ID、schema、JSON 值或会话限额常量。ID 与 schema/value 校验中的固定限制见下文；协议对契约数量、方法数量、调用期限、消息数和回调数的要求见 [外部扩展协议](../extensions/protocol.md)，由消费方执行，本模块尚未为这些要求提供公共限额或会话校验 API。

## 身份与声明类型

`ExtensionKind`、`ExtensionMetadata`、`ContractDeclaration` 和 `MethodDeclaration` 均支持 `serde::Serialize` 与 `serde::Deserialize`。三个声明结构拒绝未知字段；字段没有注明默认值时必须出现。声明中的 ID、版本、列表数量、重复项和 schema 有效性需要消费方显式检查。

### `ExtensionKind`

| Rust 变体 | JSON 字符串 |
| --- | --- |
| `ExtensionKind::Node` | `"node"` |
| `ExtensionKind::Plugin` | `"plugin"` |

身份类型决定协议角色；声明 `Node` 或 `Plugin` 不产生调用权限。该枚举支持 `Clone`、`Copy`、`Debug`、`PartialEq` 和 `Eq`。

### `ExtensionMetadata`

| 公开字段 | Rust 类型 | JSON / 默认规则 |
| --- | --- | --- |
| `protocol_version` | `u32` | 协议主版本；必须出现。 |
| `id` | `String` | 扩展逻辑 ID；必须出现。 |
| `kind` | `ExtensionKind` | `"node"` 或 `"plugin"`；必须出现。 |
| `contracts` | `Vec<ContractDeclaration>` | 契约声明数组；必须出现。 |
| `capabilities` | `Vec<String>` | 缺省为 `[]`；显式 `null` 不等同于缺省。 |
| `workspaces` | `Vec<String>` | 缺省为 `[]`；显式 `null` 不等同于缺省。 |

序列化时包含 `capabilities` 和 `workspaces`，即使它们为空。结构支持 `Clone`、`Debug` 和 `PartialEq`；相等比较包含所有字段与列表顺序，可供消费方检查重新握手时声明是否改变。

### `ContractDeclaration`

| 公开字段 | Rust 类型 | 含义 |
| --- | --- | --- |
| `id` | `String` | 契约 ID。 |
| `version` | `u32` | 契约版本，与协议主版本分开。 |
| `methods` | `Vec<MethodDeclaration>` | 方法声明。 |

### `MethodDeclaration`

| 公开字段 | Rust 类型 | 含义 |
| --- | --- | --- |
| `name` | `String` | 方法名。 |
| `read_only` | `bool` | 提供方声明的方法效果属性，不是权限或真实执行事实。 |
| `input_schema` | `serde_json::Value` | 输入 schema；反序列化仅读取 JSON 值，应显式调用 `validate_schema`。 |
| `output_schema` | `serde_json::Value` | 输出 schema；反序列化仅读取 JSON 值，应显式调用 `validate_schema`。 |

`ContractDeclaration` 与 `MethodDeclaration` 均支持 `Clone`、`Debug` 和 `PartialEq`。

## `Message`

`Message` 是支持 `Clone`、`Debug`、`Serialize` 和 `Deserialize` 的枚举。JSON 使用顶层字符串字段 `type` 作为内部标签，变体名转为 snake_case，拒绝未知消息类型和 envelope 字段。

| Rust 变体 / JSON `type` | 精确 Rust 字段 |
| --- | --- |
| `Hello` / `"hello"` | `protocol_version: u32`, `expected_id: String`, `kind: ExtensionKind` |
| `Ready` / `"ready"` | `metadata: ExtensionMetadata` |
| `Call` / `"call"` | `id: String`, `contract: String`, `version: u32`, `method: String`, `params: serde_json::Value`, `timeout_ms: u64` |
| `Result` / `"result"` | `id: String`, `result: serde_json::Value` |
| `Error` / `"error"` | `id: String`, `code: String`, `message: String`, `outcome: Outcome` |
| `Callback` / `"callback"` | `id: String`, `parent_id: String`, `method: String`, `params: serde_json::Value` |
| `Cancel` / `"cancel"` | `id: String` |

`Ready.metadata` 使用 `#[serde(flatten)]`，JSON 中没有 `metadata` 包装层。`capabilities`、`workspaces` 的缺省规则仍适用：

```json
{"type":"ready","protocol_version":1,"id":"observation-node","kind":"node","contracts":[]}
```

其余变体字段没有默认值。`params` 与 `result` 是任意 JSON 值；消费方使用已确认的契约 schema 和业务规则解释它们。未知 envelope 字段与这些 JSON 值内部的业务属性是不同层次，业务属性是否允许由 schema 决定。

各消息的角色如下：`Hello/Ready` 建立身份与声明；`Call` 发起契约方法调用；`Result/Error` 返回与 ID 关联的终态载荷；`Callback` 使用 `parent_id` 关联顶层调用；`Cancel` 请求取消指定调用。`timeout_ms` 表示相对毫秒期限。ID 关联、超时范围、角色允许发送的消息、重复回调、取消结果以及声明兼容性均由消费方维护。

反序列化只检查 serde 定义的消息结构、字段类型、必需字段和未知字段，不自动执行 `valid_id`、版本匹配、帧/声明/调用限额、`validate_schema`、`validate_value`、注册或授权。直接构造 Rust 值同样不能证明这些条件已满足。

## 结果语义与错误类型

### `Outcome`

该枚举支持 `Clone`、`Copy`、`Debug`、`PartialEq`、`Eq`、`Serialize` 和 `Deserialize`，使用 snake_case JSON 字符串。

| Rust 变体 | JSON | 协议语义 |
| --- | --- | --- |
| `Rejected` | `"rejected"` | 确认请求代表的持久操作尚未开始而拒绝请求。 |
| `Unknown` | `"unknown"` | 请求可能已经派发或产生副作用，但结果尚不能确认；保留关联身份供消费方核实。 |
| `Cancelled` | `"cancelled"` | 提供方返回取消状态；不能据此证明已产生的副作用撤销或执行者已经停止。 |

v1 中没有 `Failed` 变体。已确认的业务执行失败由具体契约的 `Message::Result.result` 表达；不能将它改写为“尚未开始”的 `Rejected`，也不能将不确定结果冒充已确认失败。消费方在无法确认副作用时保留 Unknown；发送 `Cancel`、连接关闭或反序列化成功都不会自动产生确定取消事实。

### `ExtensionError`

`ExtensionError` 支持 `Debug`、`std::fmt::Display` 与 `std::error::Error`，不实现 `serde::Serialize` 或 `serde::Deserialize`。它是 Rust 校验/消费接口的错误类型，与线协议 `Message::Error` 分开；本模块不自动在两者之间转换。

| Rust 变体 | 数据 | 含义 |
| --- | --- | --- |
| `Configuration(String)` | 原因字符串 | 扩展配置无效。 |
| `Unavailable(String)` | 原因字符串 | 扩展或连接不可用；该名称本身不证明派发后的操作没有副作用。 |
| `Protocol(String)` | 原因字符串 | 协议违反或关联异常；消费方仍须结合派发阶段判定业务结果是否未知。 |
| `Rejected(String)` | 原因字符串 | 请求/输入被拒绝；本模块 schema 校验失败也返回此变体。 |
| `Unknown { call_id: String, message: String }` | 调用身份与原因 | 调用结果未知。字段是普通字符串，构造该错误不会检查 ID 或建立持久核实记录。 |
| `Cancelled` | 无 | 派发前已取消。与提供方在线协议中返回 `Outcome::Cancelled` 分开。 |

## ID 函数

```rust
pub fn valid_id(value: &str) -> bool;
pub fn call_id() -> String;
```

`valid_id` 仅当字符串非空、UTF-8 字节数不超过 128，且全部字节属于 ASCII 字母、数字、`.`、`_`、`-` 时返回 `true`。它检查词法规则，不检查命名空间归属、是否重复、是否已登记、资源存在性或权限。

`call_id` 返回 `<进程ID>-<Unix时间纳秒>-<进程内计数器>` 格式的字符串。计数器从 1 开始，通过 `AtomicU64` 递增；时间读取失败时使用零时长。该函数不维护跨机器或进程重启后的全局唯一性，不生成认证令牌，也不授予授权。消费方仍负责调用 ID 的使用范围与不重用要求。

## Schema 与 JSON 值校验

```rust
pub fn validate_schema(
    schema: &serde_json::Value,
) -> Result<(), ExtensionError>;

pub fn validate_value(
    schema: &serde_json::Value,
    value: &serde_json::Value,
) -> Result<(), ExtensionError>;
```

`validate_schema` 检查已解析的 JSON 值是否属于当前支持的严格 schema 子集。`validate_value` 先校验 schema，再检查整个值的递归深度与大小，并按 schema 校验值。失败均返回 `ExtensionError::Rejected`；这两个函数不读取帧、不解析原始 JSON、不注册契约，也不执行业务调用。

### 支持的 schema

schema 必须是 JSON 对象并具有一个字符串 `type`。支持 `object`、`array`、`string`、`integer`、`number`、`boolean`、`null`；`integer` 接受 `serde_json::Value` 中的有符号或无符号整数，不把浮点表示的 `1.0` 当作整数。联合类型及其他类型拒绝。

| 关键字 | 当前校验与消费行为 |
| --- | --- |
| `type` | 必需，且为上述一个字符串类型。 |
| `properties` | 仅用于 object；必须为对象，各属性值递归校验为 schema。 |
| `required` | 仅用于 object；必须为字符串数组，各名称须存在于 `properties`；校验值时要求对应属性存在。 |
| `additionalProperties` | 仅用于 object；只接受布尔值。`false` 拒绝未声明的值属性；省略或 `true` 允许。 |
| `items` | 仅用于 array；必须为一个有效 schema，校验每个元素。省略时不执行元素类型/schema 约束，整体深度与大小仍受限。 |
| `enum` | 非空数组，最多 64 项；值必须与数组中的一个 `serde_json::Value` 相等。 |
| `maxLength` | 仅用于 string；非负 `u64`，按 `text.chars().count()` 检查 Unicode 标量值数量，不按 UTF-8 字节或用户感知字形计数。 |
| `maxItems` | 仅用于 array；非负 `u64`，检查元素数量。 |
| `minimum`、`maximum` | 仅用于 integer/number；合法数值边界，执行包含端点的上下界比较。 |
| `description` | 必须为字符串，不参与值匹配。 |

所有未列出的关键字均拒绝，包括 `$ref`、`oneOf`、`anyOf`、`format` 和 `pattern`。本模块不宣称完整 JSON Schema 兼容；它没有额外检查 `required/enum` 重复项、enum 成员与声明类型是否一致，或 `minimum <= maximum`。消费方需要更严格的声明一致性规则时应另行检查。

### 固定校验限额

| 对象 | 当前限制 | 计数方式 |
| --- | --- | --- |
| 单 schema | 64 KiB | `schema.to_string().len()`，即紧凑 JSON 序列化后的 UTF-8 字节数。 |
| schema 深度 | 最大深度索引 12 | 根 schema 为 0，经 `properties` 子 schema 或 `items` 递归时加 1；这是 schema 层数规则。 |
| `properties` | 最多 64 个 | 属性条目数。 |
| `required` | 最多 64 项 | 数组项数。 |
| `enum` | 1 至 64 项 | 数组项数。 |
| 单 JSON 值 | 256 KiB | `value.to_string().len()`，即紧凑 JSON 序列化后的 UTF-8 字节数。 |
| JSON 值深度 | 最大深度索引 24 | 根值为 0，所有对象属性值/数组元素递归加 1，包括 schema 未声明的属性和没有 `items` 的元素。 |

这些大小检查针对已解析值的重新序列化结果，不等同于原始输入帧长度；原始空白、JSON 转义形式及 envelope 开销仍由传输帧限制覆盖。校验函数不会替消费方限制解析之前的输入。

数值 schema 带有 `minimum` 或 `maximum` 时，边界和受校验数值必须是有限值，并位于 `[-2^53, 2^53]`，即 `[-9007199254740992, 9007199254740992]`；超出即拒绝，避免用浮点比较大整数时丢失边界精度。未带上下界的 integer/number 不施加该范围限制，仍受 `serde_json::Value` 的可表示范围约束。

## 使用示例

以下示例展示 Host 内如何显式校验载荷并构造/解析消息，不建立网络会话。它不是外部节点接入方式；节点应按 JSON 规范独立实现。宿主工具链要求见 [Cargo.toml](../../Cargo.toml) 与 [rust-toolchain.toml](../../rust-toolchain.toml)。

```rust
use recuvora_host::protocol::{
    ExtensionKind, MAX_FRAME_BYTES, Message, PROTOCOL_VERSION,
    call_id, valid_id, validate_schema, validate_value,
};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let schema = json!({
        "type": "object",
        "properties": {
            "target_id": {"type": "string", "maxLength": 128}
        },
        "required": ["target_id"],
        "additionalProperties": false
    });
    validate_schema(&schema)?;
    let params = json!({"target_id": "target-001"});
    validate_value(&schema, &params)?;

    let hello = Message::Hello {
        protocol_version: PROTOCOL_VERSION,
        expected_id: "observation-node".into(),
        kind: ExtensionKind::Node,
    };
    let hello_bytes = serde_json::to_vec(&hello)?;
    if hello_bytes.len() > MAX_FRAME_BYTES {
        return Err("hello frame exceeds the protocol limit".into());
    }

    let id = call_id();
    let contract = "com.example.observation";
    let method = "query";
    if !valid_id(&id) || !valid_id(contract) || !valid_id(method) {
        return Err("invalid call routing ID".into());
    }
    let call = Message::Call {
        id,
        contract: contract.into(),
        version: 1,
        method: method.into(),
        params,
        timeout_ms: 30_000,
    };
    let bytes = serde_json::to_vec(&call)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err("call frame exceeds the protocol limit".into());
    }
    let decoded: Message = serde_json::from_slice(&bytes)?;
    if let Message::Call { params, .. } = decoded {
        validate_value(&schema, &params)?;
    }
    Ok(())
}
```

用于真实消费前，还应按 [外部扩展协议](../extensions/protocol.md) 核验握手身份/版本、契约登记与兼容、可信调用白名单、期限/容量、响应关联和输出 schema；这些条件不是上述序列化或校验函数的自动效果。
