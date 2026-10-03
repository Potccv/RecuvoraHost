# Host 源码

| 模块 | 职责 |
| --- | --- |
| [control](control/README.md) | 故障活动事实、监控检查点及其提交与重放 |
| [boot](boot/README.md)、[configuration](configuration/README.md) | 可信服务装配、配置加载和控制路径保护 |
| [application](application/README.md) | 应用服务、操作受理与持久回执、取消和共享服务生命周期 |
| [recovery](recovery/README.md) | Core 能力注入、恢复聚合提交、当前故障门、目标所有权和受管调度 |
| [runtime](runtime/README.md) | 通用取消与在途调用监督；可信模块生命周期、依赖、服务和事件框架 |
| [persistence](persistence/README.md) | Core 与故障台账提案的可靠提交、配置绑定与当前日志恢复 |
| [monitoring](monitoring/README.md) | Host 定时轮询、规则、时效、覆盖、发现与故障联动 |
| [harnesses](harnesses/README.md) | Harness 注册、选择、会话/项目/工具接口约定与校验 |
| [protocol](protocol/README.md) | Host 的消息、schema、ID 与限额实现；外部节点按文档独立接入 |
| [integrations](integrations/README.md) | 协议会话、ExtensionRegistry、远端 Harness/监控/修复后端 |
| [actions](actions/README.md)、[repair](repair/README.md)、[simulation](simulation/README.md) | 独立本机文本动作、人工工具流程和模拟测试 |
| [cli](cli/README.md)、[server](server/README.md)、[presentation](presentation/README.md) | 操作入口、认证授权和事实视图 |

程序入口为 [main.rs](main.rs)，库导出以 [lib.rs](lib.rs)为准；`cli` 与 `presentation` 为内部模块，其余模块提供公开服务。接口概览见 [Host Rust API](../docs/api/rust.md)。

故障状态通过本库 control 管理，审批、任务与知识使用 Core 公共引擎及提案确认接口。Host 的协议类型统一来自 `crate::protocol`；对外契约以 [扩展协议 v1](../docs/extensions/protocol.md)为准，不要求节点依赖 Rust 包。修改前阅读 [源码规范](AGENTS.md)和目标模块局部规则。

HTTP 通过 application 使用已装配的服务，配置不依赖 boot。恢复应用接口以 `recovery` 为规范入口，具体 Node/Harness 后端保留在 `integrations::recovery`；该适配目录继续转导出恢复接口以兼容已有嵌入调用。
