# 实现状态

当前 Host 使用 Core 0.2.0 的完整恢复引擎；control 只维护故障台账。Host 实现能力接口、聚合持久化、取消监督和共享目标所有权。HOST-001 与 HOST-002 已修复。HOST-003 已接入统一修复协议；当前恢复配置版本为 2，不提供旧协议导入。验证结果只覆盖下文实际运行的 Host 检查。

## 已实现能力

| 范围 | 能力 |
| --- | --- |
| 应用边界 | 单 Cargo 包、Core 业务计算接口与 Core、Host 协议模块、配置安全准备、CLI、回环认证 HTTP |
| 持久化 | 可信配置与完整事务绑定、预期 revision 原子比较、提交 ID 内容冲突拒绝、同步后确认、文件身份/锁/容量保护、领域历史恢复 |
| 扩展与 Harness | ws/wss/http/https、身份/schema/白名单、独立执行和审核容量、回调分类及 Unknown 保留、派发后无效结果不解释为可重试拒绝 |
| 监控 | 只读轮询、规则、时效、覆盖、动态发现、故障与检查点同次持久提交 |
| 自动恢复 | 显式 `repair_with_harness` 会话委托、Host 工具执行、Core 聚合提案、具体动作提交后派发、共享目标所有权、独立验收与原子 Unknown 核实 |
| 经验交付 | 独立无工具总结与脚本化评估、64 KiB 有界总结输入、稳定经验身份、三次自动尝试及显式重试，失败/Unknown 隔离先于知识保存，失败不重执行 |
| 管理与本机文本修复 | 恢复任务/审批/知识查询、认证人工决定、暂停恢复、绑定节点结果核实、受限 Windows 文本动作、模拟及持久应用回执 |
| 外部 UI | 固定资产快照、身份与权限、只读插件 catalog/view、独立页面目录与显式描述刷新、目标记录查询 |

Host 不启动节点或提供方子进程，不提供原始日志上传、任意脚本提交、配置热更新或供应商线程续接。本机文本修复与自动恢复仍使用独立接口和状态目录。

## 应用服务分层

Host 保持单 Cargo 包。application 持有应用服务、操作回执、取消和关闭；boot 装配能力，configuration 加载配置并保护控制路径；server 负责认证和 HTTP 映射。恢复服务位于顶层 recovery，内部区分聚合存储、Core Platform、派发门与调度；integrations/recovery 保留节点、Harness、执行器和总结输入适配。恢复、审批和经验仍使用同一聚合提交，日志、配置与线协议格式不变。

旧 boot::host 和 integrations::recovery 的公开服务路径保留转导出。动作准备改由恢复专用 RepairActionGuard 提供，自定义后端使用 AuthorizedRepair::repair_action_guard；通用 dispatch_guard 只复核与释放发送保护，调用约定见 [Rust API](api/rust.md)。结果核实复用启动时已绑定的同一节点后端。独立文本修复、模拟能力及模拟 Engine 的独立关闭责任保留。

应用服务回归不经过 HTTP，验证关闭先取消并等待回执、关闭期间拒绝新请求，以及未完成操作重开为 Unknown 且稳定 ID 不重放；原 HTTP、网络、持久化与聚合中断回归继续覆盖兼容行为。

## 问题与迁移状态

### HOST-001 · 已修复 · 回调 Unknown 分类和上层结果保留

[ExtensionClient](../src/integrations/extensions/client.rs) 保留回调 Rejected、Cancelled、Unknown 的 wire 分类；回调 Unknown 在当前调用内保持，节点后续普通 Result 或取消完成不能覆盖。回调监督失败、工具执行后超限或发送失败的回调回执、派发后的协议/连接/回执错误和不完整收尾维持 Unknown，不自动重放。[工具路由](../src/integrations/harness/callbacks.rs) 保留 Harness 不确定结果，派发后无法确认的工具输出错误不降为拒绝。

固定回归覆盖派发前取消/拒绝、工具 Unknown、回调监督失败、收尾等待与容量保留、Unknown 后顶层成功/取消及零重放；测试位于 [network_transport](../tests/network_transport.rs) 与 [remote_harness](../tests/remote_harness.rs)。

### HOST-002 · 已修复 · Harness 输入输出 schema

[Harness 路由](../src/integrations/extensions/routing.rs) 在派发前验证登记的方法输入，收到结果后验证输出，并校验保留方法的副作用声明。projects 的无效输出被拒绝；create_project、run 的派发后无效输出保留 Unknown。无效输入不占用节点派发，容量仍由监督任务持有至收尾。

[remote_harness](../tests/remote_harness.rs) 固定覆盖 projects/create_project/run 的有效声明、缺必填输入、输出超限、声明不一致和调用次数；Protocol 和网络用例检查结构化分类。

<a id="host-003"></a>

### HOST-003 · 已完成 · 业务引擎与能力适配分离

Core 管理完整恢复业务及状态；Host persistence 与恢复服务实现持久化、能力调用和运行监督。恢复配置使用 schema 2，明确区分 Core 逻辑目标及动作种类与 Host `ScriptExecutorConfig` 的平台、语言和检查查询。节点的 `recuvora.repair` 脚本线协议由 Host 适配为中立 `RepairArtifact`。Host 后端的执行器配置固定绑定日志头，重启改变平台、语言或检查范围会拒绝打开原状态目录，审核上下文包含该范围。

当前只有一条受授权 Harness 修复路径。Host 提供当前故障和观察，Core 匹配经验，具体动作持久提交后派发；独立执行回执与业务验收决定结果。模型总结与经验保存分别提交，重试不重新执行。知识接口只返回统一经验。

日志保存原配置、完整输入、提交请求及版本，文件身份、独占锁与可靠同步保护确认边界。提交结果未知时重开核实历史；重启不重发许可，不重派动作。当前故障、目标所有权、审批有效期和隔离状态持续保护到网络发送。Unknown 核实一次提交独立证据、审批核实及恢复最终事实，原动作历史隔离永久保留。

不包含旧历史导入、旧诊断/脚本复用流程或旧案例交付。当前历史仍须完整重放和显式恢复，损坏、协议版本或配置冲突拒绝打开。职责与限制见[持久化](../src/persistence/README.md)和[恢复服务](../src/recovery/README.md)。

<a id="host-004"></a>

### HOST-004 · Host 页面接入已实现，UI 跳转待实现

[插件独立页面约定 v1](extensions/pages.md)与[JSON 样例](extensions/examples/pages.json)定义 `recuvora.ui_links.v1`、只读 `describe_ui_links`、页面身份与 revision、可信基础地址绑定、外部导航对象、权限及刷新失败语义。

Host 已实现 `ui_links.enabled` 与 `entrypoints` 可信部署绑定、能力/白名单检查、固定只读描述调用、URL 目录与编码校验、运行实例快照、相同 revision 内容一致性及失败撤下链接。描述调用每插件一个容量、固定期限且无回调，初始化并发有界；关闭取消并等待在途调用，重启不复用旧快照。

`GET /api/v1/ui/catalog` 保留 schema_version 1/views，增加 external_links/link_statuses，独立页面只要求 extension.read；监控视图仍额外要求 monitor.read。`POST /api/v1/ui/plugins/{plugin_id}/links/refresh` 接受 `{}`，返回完整插件快照，不自动重试、不产生业务执行权限。配置模板见[页面扩展](../profiles/extensions.pages.example.json)。配置变更需重启，没有在线停用或热更新 API。

外部 UI 的目录消费、跳转按钮、显式刷新状态及 Web/Desktop 受控打开仍待客户端实现；插件网页、登录和业务权限仍由插件维护。

**已验证范围：** Rust 1.98.1 Windows LLVM/MinGW 环境下，格式检查、全部 Host 目标编译与测试、文档测试入口及严格 Clippy 均通过。[ui_links](../tests/ui_links.rs) 的 9 项集中测试覆盖描述与 URL 样例、配置/白名单/权限、revision 与缓存、失败撤下链接、回调拒绝、容量和取消排空，以及 HTTP 目录/显式刷新与监控视图共存；模板通过配置加载校验。文档链接、锚点、JSON 和差异空白检查通过。外部 UI 资产用例仍需单独提供产物，未执行真实网页、登录、浏览器或跨机跳转验收；Host 测试不代表 Core 自身测试。

### 接口与客户端边界

节点只读核实方法为 recuvora.repair v1 的 reconcile；check_result 是 Host 管理入口。自动恢复记录位于 /recovery，独立文本修复 /repairs 与 /approvals 不包含这些记录。HTTP 接口存在不代表官方客户端已消费全部接口。

## 自动验证

检查入口为 [Rust 检查脚本](../scripts/windows/check.rs)，范围为 Host 格式、所有目标编译、集中测试、文档测试和严格 Clippy；不运行 Core 自身测试。测试目录说明每个目标职责，持久化与恢复测试使用实际 Host 存储。

UI HTTP 检查已迁移为 Cargo 集成测试。`cargo test --locked --test ui_contract` 的默认用例通过，覆盖隔离静态资产、认证/来源/权限、模拟、重复操作拒绝和 Host 强停重启回执；外部 UI 资产用例默认 ignored，未据此验证实际 UI 客户端。

使用 Rust 1.98.1 Windows GNU LLVM 工具链运行完整 Host 检查入口，通过格式、所有目标编译、245 项顶层测试用例、文档测试命令和严格 Clippy（`-D warnings`）。其中库测试 77 项，故障领域测试 11 项；数量不重复计算父用例启动的子进程测试。另有 5 个自定义网络测试程序通过。2 项忽略为外部 UI 资产用例和由父测试显式调用的中断子入口；文档测试当前 0 项。聚合中断回归覆盖操作与审批创建、授权、回执、验收、经验交付、Unknown 核实，以及同步成功但授权确认丢失时零执行和重开核实。Host 文档相对链接、锚点、JSON 和差异空白检查通过。该入口编译 Core 生产依赖，不运行 Core 自身测试套件。

## 统一修复与独立经验

Core 引擎生成包含故障、观察、完整匹配数量、最多四条预算内经验与明确委托的统一修复请求，Host 调用 Harness；政策须显式允许 `repair_with_harness`。当前网络后端仅开放 `inspect_target` 与 `apply_repair`，单会话最多一次变更，复用节点既有脚本执行能力。具体动作先持久保存；回执丢失、后端异常或动作轨迹不符保持 Unknown，使用已提交动作进行后续核实。

业务结果由执行节点回执和独立验收确定，模型文本不产生成功事实。总结会话消费有界只读投影，完整实际动作载荷与可信结果必保留；可选字段按整字段纳入预算，省略清单也计入 64 KiB prompt 上限。发生省略时 Host 将脚本化评估设为无法判断并丢弃候选，完整任务、证据与交付身份继续保留在审计记录中。总结会话返回经验、关联经验身份以及可脚本化／不适合／无法判断；候选脚本不是已验证脚本。自动总结最多三次，每轮最多处理四个工作；显式重试和恢复保留身份与次数，不重执行修复。任务 HTTP 详情展示待交付经验状态，知识查询返回独立经验。

现有完整检查入口通过格式、所有目标编译、集中测试、文档测试命令和严格 Clippy。`repair_backend` 使用回环网络与真实 Host 持久适配，覆盖统一会话、第二次变更拒绝、无脚本经验及后续命中、总结失败预算和重启续接、执行回执丢失、后端执行后异常、动作轨迹不符以及独立核实；大输入回归验证完整任务超过 64 KiB 后仍可在重启后总结，且不重复派发动作。[summary_context](../tests/summary_context.rs) 覆盖最大动作载荷、Unicode/JSON 转义、Unknown 事实及省略元数据临界预算。提交中断回归验证当前协议。文档测试当前没有可执行用例，外部 UI 资产用例仍需单独提供产物。

当前实现没有语义经验检索、候选脚本自动晋升或多步变更会话。当前 schema 2 配置必须显式允许会话政策和受限动作；不兼容旧配置。具体接入见[恢复流程](recovery.md#统一修复与经验总结)。

## 尚需部署验收

真实脚本沙箱、执行者进程树监督、提供方认证、业务恢复、跨机部署、浏览器/Tauri 交互及长期稳定性由实际节点、UI 和部署分别验收。协议替身、文件读回、批准或 HTTP 接受不能代替这些结论。[ui_contract.rs](../tests/ui_contract.rs)使用 Rust 检查 Host HTTP 与静态资产交付，默认使用隔离资产替身，可选外部 UI 资产用例需显式运行；两者均不执行 JavaScript 客户端，不能据此宣称客户端完整支持。
