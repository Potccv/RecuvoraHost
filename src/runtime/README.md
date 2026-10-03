# 应用模块运行管理

本模块在应用中按依赖顺序启动模块、共享服务、传递事件并管理实例资源。关闭时通知模块停止，并等待正在处理的任务结束。它不解释修复规则，也不替代 Core 恢复引擎。

`operation` 提供 `Cancellation` 与 `CallScope`，在调用者丢弃 future 后仍监督在途工作；关闭先拒绝新派发，再取消并等待现有调用，超时不证明外部执行停止。

生产 Host 的共享服务由 [application](../application/README.md) 持有、[boot](../boot/README.md) 显式装配；通用 Module/服务/事件框架供可信嵌入使用，内置使用者是模拟命令。它与 operation 的调用监督分别承担责任，不是第二套恢复调度器。
