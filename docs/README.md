# Host 文档索引

从 [项目 README](../README.md)查看运行前提；已有存储升级先核对 [Core 0.2 导入限制](implementation-status.md#host-003)。修改源码先读 [开发指南](development.md)和 [架构](architecture.md)。本目录的内容分工与写作规则见 [AGENTS](AGENTS.md)。

## 使用与管理

| 文档 | 查询内容 |
| --- | --- |
| [配置](configuration.md)、[模板](../profiles/README.md) | 状态与控制路径、权限、网络 endpoint/TLS、能力初始化及外部 UI |
| [CLI](cli.md) | Harness、受限文本修复、模拟命令与退出语义 |
| [HTTP API](console-api.md) | 身份、权限、资源、revision、分页、异步回执及静态托管 |
| [监控](monitoring.md) | 定时轮询、规则、时效、覆盖、发现和故障提交 |
| [恢复流程](recovery.md) | 显式启用、故障触发、调度、暂停恢复与 Unknown 结果核实 |
| [审批](approval.md) | 审核规则、持久决定、一次许可与新旧流程差异 |

## 设计与扩展接入

| 文档 | 查询内容 |
| --- | --- |
| [架构](architecture.md) | Core、Host、节点和 UI 职责，服务接口与关闭顺序 |
| [插件](plugins.md)、[集成实现](integrations.md) | 内置模块、独立扩展、权限与各层实现归属 |
| [扩展接入](extension-protocol.md) | Host 网络客户端与节点接入要求 |
| [扩展协议 v1](extension-protocol-v1.md) | 语言无关消息、schema 子集、限额、错误和网络会话规范 |
| [节点业务接口 v1](node-contracts-v1.md) | Harness、工具回调、恢复执行器及观察的 JSON 请求/响应 |
| [协议 JSON 样例](protocol-v1-vectors.json) | 独立实现可用于兼容检查的固定消息向量 |
| [Host Rust API](host-rust-api.md)、[Protocol Rust API](protocol-rust-api.md) | 嵌入服务与宿主协议实现参考；外部节点不依赖 Rust 包 |

## 开发与验证

| 文档 | 查询内容 |
| --- | --- |
| [开发指南](development.md)、[源码导航](../src/README.md) | 环境准备、修改流程与模块职责 |
| [脚本](../scripts/README.md)、[测试](../tests/README.md) | 检查命令、集中测试、隔离资源与可选 UI 检查 |
| [实现状态](implementation-status.md)、[Core 0.2 迁移](implementation-status.md#host-003) | 当前集成能力、旧数据导入范围、迁移验收、检查证据及部署验收限制 |
| [项目规范](../AGENTS.md)、[文档规范](AGENTS.md) | 长期开发规则与文档内容归属 |
