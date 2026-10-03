# Host 应用服务

本模块承接入口与能力之间的应用协调。`Application` 持有共享服务、独立本机文本修复和封闭模拟句柄，管理应用操作回执、去重、容量、取消和关闭；它不依赖 HTTP、Axum 或 CLI。

| 文件 | 职责 |
| --- | --- |
| `mod.rs` | 应用服务组合、只读服务查询、操作接受/完成、自动恢复结果核实与关闭 |
| `host.rs` | `HostRuntime` 服务所有权、查询和按依赖顺序关闭 |
| `operations.rs` | Harness 调用、独立文本流程和封闭模拟的应用调度与回执收集 |
| `journal.rs` | 有界 `operations.jsonl`、接受记录、完成记录与重启后的 Unknown |
| `error.rs` | 与传输状态码无关的应用错误分类 |

配置读取、控制路径保护与启动由 [boot/application.rs](../boot/application.rs) 装配；HTTP [Console](../server/README.md) 保留原有公开入口，委托本模块协调应用能力。`HostRuntime` 的启动方法由 boot 装配层提供。恢复阶段与业务授权仍由 Core 决定，[recovery](../recovery/README.md) 提供 Host 持久化、能力注入及监督。

自动恢复结果核实复用启动时绑定的同一个 NodeRepairBackend，不根据 HTTP 请求创建后端或选择执行器。独立本机文本修复和封闭模拟保持各自明确的能力边界，不成为 Core 自动恢复的替代流程。

操作回执保持既有存储格式与 Unknown 语义。查询使用只读 journal guard，不能修改记录；操作接受后由应用任务收集结果，HTTP 连接结束不证明执行停止。关闭先停止接受、取消并等待应用调用，再关闭 HostRuntime。为兼容已有嵌入接口，`Console::open` 和 `boot::application::open` 仍返回独立模拟 Engine，调用方在应用关闭后关闭该 Engine。

开发规范见 [AGENTS.md](AGENTS.md)。
