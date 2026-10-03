# 扩展运行时规范

继承 [集成层规范](../AGENTS.md)。实现位置见 [README](README.md)。

- 维护 endpoint、传输客户端、插件/节点注册表、namespace、allowlist、调用容量和回调路由。
- repair 的 inspect/verify/reconcile 只读；execute_script 只允许 Core 消费许可后的可信内部路由。
- Host wire 类型统一来自本包 `crate::protocol`；外部实现遵循 Host 协议文档，不依赖本包。
- 配置启用、契约登记和业务授权必须继续分别核验。
- 不实现外部节点服务端、供应商进程或自动安装市场。
