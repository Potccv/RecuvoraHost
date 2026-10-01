# Host 测试

本目录验证应用入口、配置边界、HTTP接口约定和Core服务使用，不复制或自动运行Core自己的测试套件。目标以[Cargo.toml](../Cargo.toml)为准；需要私有宿主接口的用例仅在测试构建中引用。

| 范围 | 检查内容 |
| --- | --- |
| 开发检查工具 | `check_script.rs` 检查参数拒绝、Host/本地依赖源码保护、链接目录拒绝、失败停止、已有数据保留与调用方环境不变 |
| 协议约定 | `protocol.rs` 检查固定 JSON 消息、Ready 缺省字段、ID、schema 必需/额外属性、Unicode 长度、值深度与数值边界 |
| CLI与使用者 | help、严格参数、配置、模拟测试、Harness项目/文本选择、共同期限与Unknown输出 |
| HTTP应用 | Bearer、来源、权限、请求大小、稳定操作ID、回执、分页与重启 |
| Core恢复流程映射 | 原始任务/审批类型、`result_check` 与 `checked_at_ms`、审批与任务revision、可信actor、显式配置及容量、共享目标所有权、故障登记权威、暂停恢复与根据节点证据核实结果 |
| 监控与故障 | 摘要/详情、确认revision、关联修复、无权限及陈旧状态 |
| 插件与观测记录 | 只读描述、schema/限额、配置绑定、网络来源、游标与限制结果数量的查询 |
| 外部静态文件 | 固定允许表、编码越界/未知路径、认证隔离、启动快照、缺失/超限及未配置 |

源码保护用例同时覆盖 Host/Core 内的新模拟数据目录在创建前被拒绝、Host 源码修复目标被拒绝，以及服务启动拒绝 Core 源码路径。生产文件边界用例保持受保护目录拒绝，HTTP 不伪造批准或执行会话；故障关联用例只验证范围、revision、权限、失败回执和重启后的来源历史。

节点替身只使用回环网络会话，测试辅助位于本项目，不读取Core测试文件。测试不连接活动节点、真实令牌或提供方账号。

Core作为依赖按生产配置编译，测试不能依赖Core自身cfg(test)的文件路径豁免。Host 维护本机文件动作的边界测试，Core 单独维护恢复决策与授权测试。`repair_backend` 使用隔离网络节点验证实际 Host 初始化、人工审批、正常执行、回执丢失后的执行结果核实、修复经验中的脚本隔离与关闭后重开；这些检查不能代替真实节点或业务验收。

## Cargo检查

先设置Host/Core源码外的CARGO_TARGET_DIR与RECUVORA_TEST_TEMP，运行：

```powershell
cargo run --locked --features dev-check --bin recuvora-host-check -- --build-dir $env:CARGO_TARGET_DIR --test-temp $env:RECUVORA_TEST_TEMP
```

也可使用`cargo test --all-targets --locked`单独运行本包测试。脚本包括格式、check、测试、文档示例和Clippy，环境与工作目录只设置在检查子进程中，不改变调用方。每个测试只清理自己记录的临时路径，失败调查后也须精确清理，不清空共享根或活动部署。

## 可选真实客户端接口约定检查

[ui_contract.mjs](ui_contract.mjs)是额外Node.js检查，参数依次为已经构建的Host程序、外部UI静态文件目录和源码外测试根：

```powershell
node ./tests/ui_contract.mjs $env:RECUVORA_HOST_BINARY $env:RECUVORA_UI_DIST $env:RECUVORA_TEST_TEMP
```

该脚本使用实际UI的api.js访问Host子进程HTTP，覆盖身份/来源/权限、模拟、重复ID、强停重启后的Unknown和静态文件字节一致性。随机临时令牌仅用于本次测试，结束后按记录路径清理。它不属于默认Cargo脚本，Node不是Host编译的必需依赖。

此检查使用真实HTTP与客户端代码，但不启动浏览器或Tauri窗口，也不调用实际模型/节点，不证明业务恢复或跨机部署。全部测试都不能把确认收到、请求接受或文本读回当作授权与恢复事实。规则见 [AGENTS](AGENTS.md)。

恢复测试使用 Core 的实际 FileTargetOwnership，并为独立用例配置各自的隔离权威目录。recovery_incident_guard 验证未绑定拒绝、不同存储同目标互斥、未完成任务重开和等待当前调用结束后的所有权转交；repair_backend 验证 Unknown 关闭后不能被新状态目录接管，原存储仍可恢复。缺少 IncidentGuard 时，新故障登记即被拒绝；HTTP 夹具显式提供限定故障身份的权威。配置加载回归确认 ownership_dir 必填、目录隔离、源码拒绝和不创建存储。

完整检查结果见[实现状态](../docs/implementation-status.md#自动验证)。协议/网络测试通过不代替实际节点或业务恢复验收。
