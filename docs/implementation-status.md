# 实现状态

## 已实现

| 范围 | 能力 |
| --- | --- |
| 应用边界 | 单 Cargo 包、Core 公共接口与 Host 内置协议模块、配置安全准备、CLI、回环认证 HTTP |
| 扩展与 Harness | ws/wss/http/https 四种连接方式/TLS、身份/schema/白名单、只读回调、独立执行与审核会话 |
| 监控 | 只读轮询、规则、时效、覆盖、动态发现与向 Core 原子提交重启后仍保留的故障 |
| 自动恢复 | 显式 recovery_config 与共享 ownership_dir、Core 原始任务/审批/修复经验类型、持久目标所有权绑定、恢复流程调度器、故障登记及执行前复核、节点 RepairBackend |
| 恢复流程管理 | 运行状态、分页任务、详情/审批、人工决定、暂停恢复、根据节点证据核实未知执行结果、修复经验搜索 |
| 兼容流程 | Windows 限制文件大小的文本修复、审批管理、模拟测试、应用操作回执与重启 Unknown |
| 外部 UI | 固定资产内存快照、身份/权限、只读插件 catalog/view 和目标记录查询 |

Core 当前只公开 operation 与 recovery。Host 不提供旧 CoreSettings、原始日志上传、任意脚本提交、节点启动、插件市场、配置热更新或线程续接。旧文本修复和新自动恢复流程有独立接口与状态目录。

## 已知问题

以下问题已通过源码检查和隔离回环节点复现，程序实现尚未修复。现有检查通过不能作为这些问题已经关闭的依据。P1 表示优先处理的结果语义问题，P2 表示需要补齐的契约边界。

### HOST-001 · P1 · 回调结果未知被转换为拒绝

**位置：** [ExtensionClient](../src/integrations/extensions/client.rs) 的 `call_inner`，以及 [Harness 工具路由](../src/integrations/harness/callbacks.rs) 的 `ToolRouter::call`。

`call_inner` 将回调处理器的所有错误发送为 `Message::Error { outcome: Rejected }`；工具路由也将所有 `HarnessError` 转成 `ExtensionError::Rejected`。因此 `Unknown` 与 `Cancelled` 的结构化分类在返回节点之前丢失。

**复现与影响：** 回调处理器返回 `Unknown`，说明 external action 已派发但回执丢失；回环节点实际收到的错误正文仍含结果未知，结构化 `outcome` 却是 `rejected`。节点随后返回顶层 `Result` 时，Client 仍可返回成功。提供方可能据此误判动作尚未开始并尝试重试；本次复现没有连接实际执行节点，也没有证明发生了重复动作。

**待处理：** 保留回调 `Unknown` / `Cancelled` / `Rejected` 的分类，对派发后的协议、连接或回执错误采用明确的 Unknown 规则；规定有副作用回调出现 Unknown 后，顶层结果和取消收尾如何保留该事实，不能用模型的普通完成结果消除未知副作用。

**关闭条件：** 固定回归覆盖派发前拒绝、派发前取消、派发后回执丢失、回调收尾失败，以及 Unknown 回调后节点返回顶层成功的情况；断言 wire 分类、上层结果与不重放行为。

### HOST-002 · P2 · Harness 调用未执行声明的输入输出 schema 校验

**位置：** [扩展路由](../src/integrations/extensions/routing.rs) 的 `call_harness_inner`。

该函数检查节点角色和方法白名单，却丢弃 `method_for` 返回的方法声明，随后直接调用 Client。与普通只读、repair 和声明式 view 路由不同，Harness 调用没有对参数和返回值执行 Protocol 的 `validate_value`。后续 DTO 与领域检查不能替代节点声明的必填字段和限额。

**复现与影响：** 节点声明输入必须含 `required_by_node`、输出数组 `maxItems: 0`。实际请求缺少该字段、节点返回一个合法项目；Protocol 分别报告 `missing required property` 与 `array too long`，Host 的 `list_projects` 却返回成功。登记成功与消息可反序列化尚不能保证 Harness 调用符合登记契约。

**待处理：** 派发前校验方法输入，收到结果后校验方法输出；有副作用方法派发后的无效结果必须保持 Unknown，不能解释成可安全重试的拒绝。补充 `projects`、`create_project`、`run` 的正反例与声明限额回归。

**关闭条件：** 无效输入不产生节点调用，无效输出不作为成功项目或会话结果；有效声明继续工作，未知副作用及容量释放规则保持一致。

### 接口名称与客户端覆盖边界

节点线协议中的只读方法为 `recuvora.repair` v1 的 `reconcile`；`check_result` 是 Host HTTP / 服务管理入口名称。[扩展协议接入](extension-protocol.md) 已校正这两个名称的说明，不能要求节点把 wire 方法改成 `check_result`。

Host 已提供 `/recovery` 管理接口，旧 `/repairs` 与 `/approvals` 不包含自动恢复记录。HTTP 接口存在不表示官方客户端已消费全部接口；客户端页面、动作名和权限的对齐情况由 UI 项目维护。客户端覆盖与上述两个实现缺口需分别核对。

## 自动验证

[Rust 检查脚本](../scripts/windows/check.rs)检查格式、所有 Host 目标编译、集中测试、文档测试和 Clippy。测试目标以 [Cargo.toml](../Cargo.toml)为准；Core 作为生产依赖编译，其测试套件独立维护。协议实现在本包，`tests/protocol.rs` 使用固定 JSON 样例检查兼容性和部分 schema 边界。

回归覆盖 HTTP 认证/权限/revision、可信 actor、重复 ID、路径保护、网络取消/断连/TLS、监控时效与故障复核、恢复调度器去重与关闭等待、独立节点执行/验收证据、Unknown 核实和修复经验。测试节点为隔离网络替身，不调用活动节点或提供方账号。各测试职责与运行要求见 [测试说明](../tests/README.md)。

目标所有权回归覆盖未绑定时拒绝提交、重复绑定拒绝、不同存储同目标互斥、未完成任务关闭后的持久所有者、原存储重开和安全排空后的转交。`repair_backend` 验证 Unknown 在关闭和更换状态目录后保持阻断；配置回归覆盖必填 `ownership_dir`、目录重叠、源码路径拒绝和加载不创建存储。未绑定 IncidentGuard 时拒绝登记新故障。

可选 [ui_contract.mjs](../tests/ui_contract.mjs)验证外部 UI 客户端与实际 Host HTTP 的基础传输、静态文件、模拟及操作回执，不属于默认检查。它不启动浏览器或 Tauri，不覆盖客户端全部自动恢复页面和 Unknown 审批核验动作。

### 最近记录的完整检查

2026-10-01 使用 Rust 1.98.1 Windows gnullvm 工具链实际运行 `recuvora-host-check`，格式、所有目标编译、集中测试、5 个自定义专项测试程序、文档测试命令（0 用例）和 Clippy 全部通过；故障注入子进程入口按既有设计忽略。新增的 7 项开发工具回归覆盖参数拒绝、源码与本地依赖目录保护、Windows junction 拒绝、普通/规范路径转换、失败停止、已有数据保留及调用方环境不变。检查脚本统一位于 [scripts/windows](../scripts/windows/README.md)，通过 `dev-check` feature 启用，普通产品构建不包含开发工具。

### 验证缺口

HOST-001 与 HOST-002 已由额外隔离探针复现，仍未修复，也未纳入固定回归套件。固定 JSON 样例不穷尽全部边界；schema 字节/节点深度和声明集合限额的完整兼容覆盖仍待补充。公共常量在 protocol、声明验证与调用监督中分别维护，现有测试通过不能证明它们始终一致。

## 尚需部署验收

真实脚本沙箱、执行者进程树监督、提供方认证、业务恢复、跨机部署、浏览器/Tauri 交互及长期稳定性必须由实际节点、UI 和部署分别验收。协议替身、文件读回、批准或 HTTP 接受均不能代替这些结论。
