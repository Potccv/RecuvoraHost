# 外部集成层

本目录实现 Host 拥有的外部协议运行时与远端适配器，由 boot 可信装配，运行资源归 application 的 HostRuntime 所有。

| 目录 | 实现职责 |
| --- | --- |
| [extensions](extensions/README.md) | endpoint、HTTP/WebSocket/TLS 客户端、扩展注册、allowlist、回调与容量 |
| [harness](harness/README.md) | 将外部节点映射为 Host HarnessProvider/HarnessAdapterFactory |
| [monitoring](monitoring/README.md) | 将外部只读接口约定映射为 Host ObservationSource 与发现来源 |
| [recovery](recovery/README.md) | 将修复节点和 Harness 映射为 Host RepairBackend，解释节点脚本及总结输入，返回可信能力结果 |

实现位置和接口职责见 [Host 插件边界实现位置](../../docs/extensions/integration.md)。

恢复聚合、Core 能力注入、所有权与调度归 [recovery 应用服务](../recovery/README.md)；这里不拥有领域提案提交。通用网络发送门只复核和释放派发保护，具体动作通过恢复专用能力先行持久准备。
