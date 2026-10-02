# 执行与验证能力

本目录维护受控目标操作与验证边界。本机文本执行与远端脚本代理分别实现，具体服务管理、发布、回退和业务验证由获准节点能力提供。

远端脚本派发位于 [NodeRepairBackend](../integrations/recovery/node_backend.rs)，仅使用 Host AuthorizedRepair。本目录只维护独立本机文本动作；Host 不启动解释器，节点负责目标作用域、执行监督和业务验收。接口见[修复流程](../../docs/recovery.md)。

**当前已实现 [Windows 文本动作](files.rs)：读取显式白名单文件，以及匹配完整原文后的全文替换。** Host RepairSession 在 Host 可靠保存 Core 审批许可消费提案后调用 crate 内部写入入口；最多 64 个准确相对文件名，每个既有 UTF-8 文件最多 16 KiB。该本机入口未开放 shell、创建/删除、发布或桌面动作，其文件读回也不提供业务健康结论。

`ScopedFiles` 接收可信目标、白名单和保护路径；`TextEdit` 只描述准确路径、原文与替换。准备和执行均核对实际打开句柄的路径、链接数和内容，保留句柄至同步落盘和读回核验完成。`filepath` 与 `winapi-util` 提供安全 Windows 接口，项目仍禁止 unsafe。Windows 共享模式不能阻止任意同账号恶意程序新增硬链接，仍需运行身份和文件权限边界，不把本实现称为权限沙箱。

全文写入不是事务，写中失效可能留下部分内容，必须保持 Unknown。`content_verified` 仅表示读回内容符合批准，不等于业务恢复。准确流程和限制见 [审批说明](../../docs/approval.md)。

## 职责与组织

| 文件 | 当前职责 |
| --- | --- |
| [mod.rs](mod.rs) | 保持 `actions` 的公开类型和文件作用域入口，挂接集中测试 |
| [contract.rs](contract.rs) | 文本编辑、动作回执、错误与文件大小边界 |
| [files.rs](files.rs) | 白名单作用域、准备编辑、持有文件句柄、执行与按文件大小上限读回 |
| [path_policy.rs](path_policy.rs) | 相对路径与控制目录规则，以及仅测试构建使用的临时根例外 |
| [platform.rs](platform.rs) | Windows 句柄固定、文件身份与链接核验、路径比较；其它平台明确拒绝 |

准备编辑和执行保留在同一内部实现中；`PreparedEdit` 字段私有，写入口仍只供 crate 内可信流程使用。路径规则与平台机制的拆分不增加动作种类、权限或新的公开执行入口。

## 输入、输出与权限

`ScopedFiles` 的目标、白名单和保护路径来自可信配置，模型只能提交范围内的 `TextEdit`。`prepare` 和 `PreparedEdit::execute` 为 crate 内部入口；[文本修复服务](../repair/README.md)准备并核验完整编辑内容，完成审批与一次许可消费后才调用写入入口。应用、依赖源码、安装目录、配置和状态均属于保护路径，不能成为修复目标。

写入前再次检查原文及文件身份。`ActionReceipt` 返回路径、写入字节数和 `content_verified`；这些字段只说明文件操作结果，不产生业务健康或恢复成功事实。

## 失败与验证边界

准备失败和目标变化拒绝写入；写入、同步或读回过程中出错时返回 `ActionError::Unknown`，不能据错误响应自动重试。审批、同目标互斥与结果核实由文本修复服务处理，本模块不启动进程、不管理插件或提供桌面操作。

回归使用隔离文件和故障状态，范围见[测试说明](../../tests/README.md)与[实现状态](../../docs/status.md)。远端脚本、节点进程监督和业务验收由恢复后端接入，其约定见[恢复流程](../../docs/recovery.md)。

开发约束见 [AGENTS.md](AGENTS.md)；授权与恢复语义见 [插件规范](../../docs/extensions/README.md) 和 [架构设计](../../docs/architecture.md)。
