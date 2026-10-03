# 模拟测试引擎

Host `simulation` 验证固定无副作用任务的调度、目标互斥、取消、超时与重启后恢复记录。类型从本模块导出；它不属于 Host 自动恢复流程，没有 recovery 根路径兼容导出。

| 文件 | 职责 |
| --- | --- |
| `mod.rs` | 公开接口约定导出与域内 actor 命令 |
| `contract.rs` | 固定模拟输入、任务状态、快照、容量与结构化错误 |
| `engine.rs` | 引擎打开、重启处理、服务句柄与关闭所有权 |
| `actor.rs` | 统一任务调度、任务队列、同目标互斥、取消收尾和持久迁移 |
| `journal.rs` | 单写锁、快照日志回放、合法迁移校验与同步追加 |
| `worker.rs` | 固定无副作用模拟阶段及回执丢失模拟 |

模拟不调用 Harness、命令、文件修复或真实平台，没有可注入执行器。`authorize_simulation()` 只允许固定模拟输入，不能产生正式审批或执行许可。执行后的取消、超时或回执丢失保留 Unknown，同目标继续阻断；重启不会重放任务。

本目录是同一个 Cargo 包中的模拟测试子域，各实现文件不能独立初始化为多个独立调度器。测试集中在 [recovery.rs](../../tests/recovery.rs) 和 [process_recovery.rs](../../tests/process_recovery.rs)，CLI 见[命令说明](../../docs/cli.md)。

`EngineConfig` 可序列化和反序列化，缺失字段采用既有默认值并拒绝未知字段；公开 `validate` 与 `Engine::open` 共用原有并发、队列、任务和日志容量边界。
