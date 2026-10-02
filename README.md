# RecuvoraHost

RecuvoraHost 是基于 Recuvora Core 的 Rust 应用宿主，提供命令行和认证 HTTP API，连接外部节点与插件，完成只读监控、故障管理和显式启用的恢复流程。项目以单 Cargo 包提供 `recuvora-host` 程序与 `recuvora_host` 库。

Host 负责配置、认证、服务装配、定时器与网络路由；Core 负责故障、审批、一次性执行许可、恢复状态和修复经验。节点负责目标采集、执行者监督与业务验收；UI 通过 `/api/v1` 使用服务，Host 可托管其外部 Web 构建资产。职责和生命周期见 [架构](docs/architecture.md)。

## 当前范围

Host 已接入 Core 0.2 的公开领域状态机，由 Host 承接持久提交、取消监督与恢复编排。提供保留原任务身份的受校验离线导入；适用范围见 [HOST-003](docs/status.md#host-003)。

- 通过 ws/wss/http/https 接入实现扩展协议 v1 的节点和插件，使用远端 Harness 发送 AI 请求、管理项目和受控工具。
- 只读轮询、规则/时效/覆盖判定、动态发现与持久故障查询；监控和故障确认均不触发修复。
- 通过显式 `recovery_config` 启动诊断、独立审核、节点执行、业务验收及修复经验管理；管理入口为 `/recovery`。
- 保留 Windows 白名单内有界 UTF-8 文件替换的旧人工修复流程，以及不操作真实目标的模拟。
- 提供单操作员 Bearer 认证、权限与 Origin 检查、持久操作回执和固定官方静态资产托管。

本项目不提供节点服务端、供应商子进程启动、前端构建或插件安装包管理。网络节点必须已经运行并兼容本项目协议；任意 REST/模型地址及 stdio/SSH stdio 节点不能直接接入。

回调 Unknown 保留及 Harness 输入输出 schema 校验已补齐，已知限制和本次验证见 [实现状态](docs/status.md)。协议测试不证明真实目标恢复、跨机部署或客户端完整支持。

## 从源码运行

以下命令使用 Core 0.2 及新的 Host 存储格式。旧日志不会在启动时自动转换；已有部署升级前须核对 [导入限制](docs/status.md#host-003)，保留原数据和稳定目标所有权。

需要 [工具链配置](rust-toolchain.toml)指定的 Rust 1.98.1，以及 [Cargo.toml](Cargo.toml)登记的 `recuvora-core` 路径依赖。先将 `CARGO_TARGET_DIR` 设置为源码外的专用绝对构建路径：

```powershell
cargo build --locked --release
```

1. 在源码外准备状态目录、活动配置与随机令牌文件。
2. 复制 [最小服务模板](profiles/server.example.json)，替换绝对路径占位符并配置最小权限。字段与令牌要求见 [配置](docs/configuration.md)和 [HTTP 身份说明](docs/api/http.md#启动与身份)。
3. 将 `RECUVORA_SERVER_CONFIG` 设置为活动配置的绝对路径，从项目根启动：

```powershell
cargo run --locked -- serve --config $env:RECUVORA_SERVER_CONFIG
```

使用已构建程序时运行 `recuvora-host serve --config PATH`。以模板端口为例，设置客户端变量 `RECUVORA_API_TOKEN` 后查询服务概况：

```powershell
Invoke-RestMethod -Uri 'http://127.0.0.1:7431/api/v1/bootstrap' -Headers @{ Authorization = "Bearer $env:RECUVORA_API_TOKEN" }
```

`ui_dir` 为空时只提供 API；加载可信官方 Web 构建目录后可访问 `/`。HTTP 仅绑定回环，跨机访问由部署方提供 TLS 反向代理或 SSH 转发。自动恢复另需 [恢复服务模板](profiles/server.recovery.example.json)及相关能力配置，普通启动不默认派发恢复任务。

批准、请求接受和动作回执分别表示不同事实。执行前由 Core 复核完整操作并发放一次许可；执行和审核使用独立会话。`Unknown` 表示外部执行结果无法确定，不能自动重放或通过更换状态目录绕过。完整规则见 [审批](docs/approval.md)与 [恢复流程](docs/recovery.md)。

统一恢复模板采用带经验参考的 Harness 修复会话，业务结果确认后独立总结经验并评估可选脚本。原脚本委托不自动扩大权限，配置和兼容路径见[恢复流程](docs/recovery.md)。

## 文档与开发

[文档索引](docs/README.md)按使用、设计和扩展接入组织详细说明：

| 需求 | 入口 |
| --- | --- |
| 运行管理命令 | [CLI](docs/cli.md) |
| 接入客户端与管理恢复任务 | [HTTP API](docs/api/http.md) |
| 配置服务与能力 | [配置](docs/configuration.md)、[模板](profiles/README.md) |
| 实现独立节点或插件 | [扩展接入](docs/extensions/connection.md)、[协议 v1](docs/extensions/protocol.md)、[节点业务接口](docs/extensions/nodes.md) |
| 嵌入 Rust 服务或修改源码 | [Rust API](docs/api/rust.md)、[源码导航](src/README.md)、[开发指南](docs/development.md) |
| 核对 Core 0.2 迁移、已知问题与验证限制 | [实现状态](docs/status.md)、[迁移范围与验收](docs/status.md#host-003)、[测试](tests/README.md) |

贡献者与编码 Agent 遵循 [AGENTS.md](AGENTS.md)及修改目录的局部规范。检查命令和环境准备见 [开发指南](docs/development.md)，检查脚本见 [scripts](scripts/README.md)。
