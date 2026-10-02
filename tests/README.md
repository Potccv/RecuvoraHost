# Host 测试

本目录验证应用入口、配置边界、HTTP接口约定和Core服务使用，不复制或自动运行Core自己的测试套件。目标以[Cargo.toml](../Cargo.toml)为准；需要私有宿主接口的用例仅在测试构建中引用。

| 范围 | 检查内容 |
| --- | --- |
| Host 持久提交 | `persistence.rs` 验证提交版本与内容冲突、配置绑定、跨进程锁、恢复 Unknown、实际动作隔离与经验幂等 |
| 开发检查工具 | `check_script.rs` 检查参数拒绝、Host/本地依赖源码保护、链接目录拒绝、失败停止、已有数据保留与调用方环境不变 |
| 协议约定 | `protocol.rs` 检查固定 JSON 消息、Ready 缺省字段、ID、schema 必需/额外属性、Unicode 长度、值深度与数值边界 |
| Host 持久化 | `persistence.rs` 检查 CAS、提交内容与配置绑定、独立进程排他写锁、尾记录损坏保留及审批/故障/知识恢复；lib 中 `persistence_faults.rs` 验证已同步但确认丢失时的 Unknown 与停止写入 |
| 跨域中断 | lib 中 `recovery_commits.rs` 在原操作保存、审批创建、许可消费、恢复执行授权、审批执行完成、业务验收和知识持久化后退出独立子进程，重开真实 Host 日志核对原身份、无隐式重执行及经验交付去重 |
| CLI与使用者 | help、严格参数、配置、模拟测试、Harness项目/文本选择、共同期限与Unknown输出 |
| HTTP应用 | Bearer、来源、权限、请求大小、稳定操作ID、回执、分页与重启 |
| Core恢复流程映射 | 原始任务/审批类型、`result_check` 与 `checked_at_ms`、审批与任务revision、可信actor、显式配置及容量、共享目标所有权、故障登记权威、暂停恢复与根据节点证据核实结果 |
| 监控与故障 | 摘要/详情、确认revision、关联修复、无权限及陈旧状态 |
| 插件与观测记录 | 只读描述、schema/限额、配置绑定、网络来源、游标与限制结果数量的查询 |
| 插件独立页面 | `ui_links.rs` 检查可选能力、地址绑定、描述/revision 校验、目录权限、显式刷新、容量、回调拒绝及关闭；使用隔离协议替身，不执行网页或客户端 |
| 外部静态文件 | 固定允许表、编码越界/未知路径、认证隔离、启动快照、缺失/超限及未配置 |

源码保护用例同时覆盖 Host/Core 内的新模拟数据目录在创建前被拒绝、Host 源码修复目标被拒绝，以及服务启动拒绝 Core 源码路径。生产文件边界用例保持受保护目录拒绝，HTTP 不伪造批准或执行会话；故障关联用例只验证范围、revision、权限、失败回执和重启后的来源历史。

节点替身只使用回环网络会话，测试辅助位于本项目，不读取Core测试文件。测试不连接活动节点、真实令牌或提供方账号。

`network_transport` 固定验证回调 wire 分类、派发前取消零调用、派发后断连/协议错误不重放、Unknown 不被顶层成功覆盖、取消排空保留未知回调证据及收尾失败。`remote_harness` 覆盖工具 Unknown/Cancelled/Rejected 与输出超限，并对 projects/create_project/run 检查必填输入零派发、输出声明限额、有效声明和容量释放；网络替身不能证明真实业务恢复。

派发门回归验证网络握手后的最终复核、拒绝零派发和发送后立即释放；`recovery_incident_guard` 使用实际 MonitorIncidentGuard 和 HTTP 替身验证门内过期证据不能派发，观察/确认提交等待或拒绝忙态，定时器不会因持门阻塞。

Core作为依赖按生产配置编译，测试不能依赖Core自身cfg(test)的文件路径豁免。Host 维护本机文件动作的边界测试，Core 单独维护恢复决策与授权测试。`repair_backend` 使用隔离网络节点验证实际 Host 初始化、人工审批、正常执行、回执丢失后的执行结果核实、修复经验中的实际产物隔离与关闭后重开；这些检查不能代替真实节点或业务验收。

## Cargo检查

先设置Host/Core源码外的CARGO_TARGET_DIR与RECUVORA_TEST_TEMP，运行：

```powershell
cargo run --locked --features dev-check --bin recuvora-host-check -- --build-dir $env:CARGO_TARGET_DIR --test-temp $env:RECUVORA_TEST_TEMP
```

也可使用`cargo test --all-targets --locked`单独运行本包测试。脚本包括格式、check、测试、文档示例和Clippy，环境与工作目录只设置在检查子进程中，不改变调用方。每个测试只清理自己记录的临时路径，失败调查后也须精确清理，不清空共享根或活动部署。

## UI HTTP 接口约定检查

[ui_contract.rs](ui_contract.rs)由 Cargo 登记并编译 Host 程序，以 Rust HTTP 客户端访问独立 Host 子进程。默认用隔离静态文件替身检查身份/来源/权限、模拟、重复 ID、强停重启后的 Unknown，以及完整允许表中静态文件的字节一致性；包含在默认 Cargo 检查中，无需 Node.js：

```powershell
cargo test --locked --test ui_contract
```

先按上文设置外部构建目录和测试根。可选外部资产用例默认 ignored，显式设置 `RECUVORA_UI_DIST` 为已构建的 UI 静态文件目录后运行：

```powershell
cargo test --locked --test ui_contract external_ui_assets_and_http_contract -- --ignored --exact
```

测试只启动本次构建的 Host，使用随机临时令牌，并在源码外创建独立测试目录；关闭子进程后按记录路径清理。外部 UI 资产仅供读取，不触碰活动部署。

两个用例都使用真实 HTTP，但不执行 UI 的 `api.js`，不验证 JavaScript 客户端的错误转换、自动重试策略或浏览器/Tauri 交互。客户端行为由 UI 项目另行验证；这些用例也不调用实际模型/节点，不证明业务恢复或跨机部署。全部测试都不能把确认收到、请求接受或文本读回当作授权与恢复事实。规则见 [AGENTS](AGENTS.md)。

恢复测试使用 Host 的实际 FileTargetOwnership，并为独立用例配置各自的隔离权威目录。recovery_incident_guard 验证未绑定拒绝、不同存储同目标互斥、未完成任务重开和等待当前调用结束后的所有权转交；repair_backend 验证 Unknown 关闭后不能被新状态目录接管，原存储仍可恢复。缺少 IncidentGuard 时，新故障登记即被拒绝；HTTP 夹具显式提供限定故障身份的权威。配置加载回归确认 schema 2、独立 executor、ownership_dir 必填、目录隔离、源码拒绝和不创建存储。

完整检查结果见[实现状态](../docs/status.md#自动验证)。协议/网络测试通过不代替实际节点或业务恢复验收。

`repair_backend` 的统一修复场景覆盖真实 Host 适配器与回环节点替身：完整会话审批、具体动作先保存、第二次变更拒绝、独立业务验收、无脚本经验、经验再次匹配、总结失败三次停止及显式重试、重开保留总结次数、超大合法经验的 64 KiB 总结投影，以及丢失执行回执后的 Unknown 核实。内部 [summary_context.rs](summary_context.rs) 回归扫描最大动作、前提、证据和 JSON 转义的临界组合，核对整字段省略元数据也包含在 prompt 预算内。替身不证明实际提供方、脚本沙箱或真实业务恢复。
