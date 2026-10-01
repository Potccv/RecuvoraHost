# 外部集成层

本目录实现 Host 拥有的外部协议运行时与远端适配器，并由 `boot::host::HostRuntime` 创建并连接。

| 目录 | 实现职责 |
| --- | --- |
| [extensions](extensions/README.md) | endpoint、HTTP/WebSocket/TLS 客户端、扩展注册、allowlist、回调与容量 |
| [harness](harness/README.md) | 将外部节点映射为 Host HarnessProvider/HarnessAdapterFactory |
| [monitoring](monitoring/README.md) | 将外部只读接口约定映射为 Host ObservationSource 与发现来源 |
| [recovery](recovery/README.md) | 将修复节点和 Harness 映射为 Host RepairBackend，并向 Core 领域状态机提交证据 |

实现位置和接口职责见 [Host 插件边界实现位置](../../docs/extensions/integration.md)。
