# Host 源码

| 模块 | 职责 |
| --- | --- |
| [boot](boot/README.md)、[configuration](configuration/README.md) | 应用服务启动、配置加载、路径保护和关闭顺序 |
| [runtime](runtime/README.md) | 模块生命周期、依赖、服务、事件与资源释放 |
| [persistence](persistence/README.md) | Core 领域提案的可靠提交、配置绑定、日志恢复及离线旧事件导入 |
| [monitoring](monitoring/README.md) | Host 定时轮询、规则、时效、覆盖、发现与故障联动 |
| [harnesses](harnesses/README.md) | Harness 注册、选择、会话/项目/工具接口约定与校验 |
| [protocol](protocol/README.md) | Host 的消息、schema、ID 与限额实现；外部节点按文档独立接入 |
| [integrations](integrations/README.md) | 协议会话、ExtensionRegistry、远端 Harness/监控/修复后端 |
| [actions](actions/README.md)、[repair](repair/README.md)、[simulation](simulation/README.md) | 旧本机文本动作、人工工具流程和模拟测试 |
| [cli](cli/README.md)、[server](server/README.md)、[presentation](presentation/README.md) | 操作入口、认证授权和事实视图 |

程序入口为 [main.rs](main.rs)，库导出以 [lib.rs](lib.rs)为准；`cli` 与 `presentation` 为内部模块，其余模块提供公开服务。接口概览见 [Host Rust API](../docs/host-rust-api.md)。

Core 的最终状态只能通过 `recuvora_core` 公共接口访问。Host 的协议类型统一来自 `crate::protocol`；对外契约以 [扩展协议 v1](../docs/extension-protocol-v1.md)为准，不要求节点依赖 Rust 包。修改前阅读 [源码规范](AGENTS.md)和目标模块局部规则。
