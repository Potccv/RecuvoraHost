# Host 协议实现

本目录维护 Host 使用的扩展协议 v1 消息、ID、固定帧限额及严格 schema/value 校验，编译在 `recuvora-host` 内，不是独立包或节点 SDK。

- `mod.rs`：统一导出 Host 内部使用的协议接口。
- `wire.rs`：七种消息、身份与契约声明、结果分类和调用 ID。
- `schema.rs`：有界的 schema 子集与载荷校验。

外部节点/插件按[语言无关的协议规范](../../docs/extensions/protocol.md)自行实现，不依赖 Host/Core 的 Rust 类型。网络、握手关联、白名单、注册与调用监督仍在 `integrations/extensions`；schema 校验和 `read_only` 声明不产生执行权限。

集中测试见 `tests/protocol.rs`，使用文档中的固定 JSON 兼容样例；原有网络与业务测试继续验证 Host 消费边界。Rust 接口说明见[模块 API](../../docs/api/protocol.md)。
