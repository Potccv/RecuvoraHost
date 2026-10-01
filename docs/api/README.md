# Host 接口参考

- [HTTP](http.md)：操作员认证、权限、资源和客户端接入。
- [Rust 嵌入](rust.md)：Host 服务装配、调用前提与资源关闭。
- [Rust 协议实现](protocol.md)：本包的 wire 类型与校验接口。

外部插件和节点按[语言无关扩展规范](../extensions/protocol.md)独立实现，不依赖 Host Rust 包。当前能力及验收限制见[实现状态](../status.md)。
