# RecuvoraHost 开发规范

本文件面向贡献者与编码 Agent。项目入口见 [README](README.md)，修改前阅读 [架构](docs/architecture.md)、[开发指南](docs/development.md)及沿途局部 AGENTS.md；文档整理遵循 [文档规范](docs/AGENTS.md)。

## 源码与职责

- 本项目为单 Cargo 应用包 `recuvora-host`；`Cargo.toml` 统一登记依赖、程序与测试，不创建 workspace、能力子包或节点程序。
- 使用标准 `src/lib.rs`、`src/main.rs`，入口保持轻量。能力按模块组织，职责和路径见 [源码导航](src/README.md)。
- 通过 `recuvora_core` 公共接口消费恢复决策，不复制 Core 的故障、审批、许可、恢复流程或知识状态机，不导入其私有实现。
- Host 维护认证、配置、定时器、服务装配、Harness/节点路由、网络客户端、CLI 与 HTTP。具体采集、脚本执行、进程监督和业务验收由独立 Node/插件实现。
- Harness 与 executor 的 node/workspace/endpoint 解析只在 Host；Core 配置仅保存逻辑身份。
- UI 只作为外部构建资产托管，通过认证 HTTP API 使用服务；不复制 UI 源码、构建前端或在展示层产生授权与恢复事实。

## 协议与可靠性

- 跨进程消息与 schema 在 `src/protocol/` 维护，对外以 [扩展协议 v1](docs/extensions/protocol.md)和 [节点业务接口](docs/extensions/nodes.md)为准；节点按文档独立实现，不依赖共享协议源码包。
- Host 仅连接已运行的 ws/wss/http/https 节点，不启动节点或供应商子进程，不接受旧 stdio/SSH stdio 启动配置。
- 在配置、持久记录、模型/工具 JSON 和网络输入处执行相应校验；配置引用错误不得静默跳过或降级。改动协议同时更新规范、JSON 样例及相关集中测试。
- 注册、订阅、定时器与连接资源归属具体运行实例。关闭先停止新派发，再请求取消、等待在途结果并释放服务；取消、断连或丢弃 future 不证明执行者停止。
- 对消息、输出、队列、并发、超时和重试设限。关键错误保留结构化结果；不得用普通完成文本覆盖 Unknown，或对不确定副作用自动重放。

## 授权与恢复

- 监控只产生观察与故障事实，确认收到不解除故障。自动恢复必须由 `recovery_config` 或 `HostRuntime::start_recovery` 显式启用。
- 人工决定经过认证入口进入 Core；执行与审批使用独立会话、上下文和权限。配置、契约登记、模型文本及 UI 状态均不产生执行权限。
- 保留完整操作绑定、当前 revision、持久批准、一次许可和执行前复核；审批不能扩大目标或动作范围。细则见 [审批](docs/approval.md)。
- 同一规范目标共用稳定的所有权目录；非终态和 Unknown 不得通过更换状态目录绕过互斥。低层嵌入调用须显式绑定 TargetOwnership、IncidentGuard 并负责关闭，见 [Rust API](docs/api/rust.md)。
- 执行回执、文件读回和业务恢复分别记录。独立本机文本动作限 Windows 白名单内既有 UTF-8 文件的有界全文替换，不扩展为任意 shell 或发布入口。
- 产品运行时不得改写宿主、核心、扩展包、启用配置或自身更新机制；贡献者按开发任务修改源码不受此运行时限制。

## 文档与验证

- 根 README 提供定位、运行入口与导航；AGENTS 维护长期规则；专题文档维护完整说明，模块 README 维护本模块职责、配置与限制。同一事实尽量只有一个详细说明位置，其余使用相对链接。
- 项目根、`src/`、能力目录、`docs/`、`profiles/`、`tests/` 和 `scripts/` 维护 README.md 与 AGENTS.md。局部规则继承上级要求，代码改动同步更新相关文档和模板。
- 文档写当前行为，不写对话记录、工作区布局或本地绝对路径。保持目标与供应商中立，使用 target、workload、provider observation、external action 等抽象语义。
- 源码仅保留计划提交的项目文件。构建、缓存、日志、活动配置、凭据与运行数据放源码外；不操作用户保留的活动部署。
- 测试集中在 `tests/` 并由 manifest 登记；临时文件使用指定外部测试根，只清理本次创建且记录的路径，失败排查结束后清理保留文件。
- 根据改动运行相关检查；完整检查入口见 [scripts](scripts/README.md)。只报告实际运行结果，Host 测试不等于 Core 自身测试或真实节点验收。
- 纯文档整理检查相对路径、锚点、命令和字段一致性，不制造程序测试。实现状态和未解决问题集中在 [实现状态](docs/status.md)，不得把协议替身或 UI 演示写成业务恢复、跨机或平台验收。
