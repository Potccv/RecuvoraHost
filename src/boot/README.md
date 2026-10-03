# 应用服务启动

本模块分派 `serve`、`harness`、`repair`、`demo` 与 `inspect`，并装配应用服务。恢复服务由顶层 [recovery](../recovery/README.md) 的 RecoveryService 提供，恢复状态迁移由 Core 引擎计算；本库 control 维护故障台账。节点通信通过 Host ExtensionRegistry，不管理节点进程。

`host.rs` 实现 HostRuntime 的启动、恢复服务打开和显式调度装配；实例字段、服务查询与关闭归 [application](../application/README.md)。HostConfig、MonitoringHostConfig 与配置加载/路径保护归 [configuration](../configuration/README.md)，原 `boot::host` 公开入口保留兼容重导出。应用服务不反向依赖 boot；装配层负责把依赖连接起来。

`application.rs` 接收不含 HTTP 类型的 ApplicationConfig，组合共享服务、独立文本修复、应用回执和模拟句柄。入口必须显式传入令牌、UI 资产与活动入口配置等控制路径；装配同时验证这些路径与恢复状态、所有权及文本目标的隔离。返回的 Application 管理受管服务，独立模拟 Engine 仍由调用方在应用关闭后关闭。

Host 在任何状态目录创建或日志打开前检查 Host 与 Core 源码边界，拒绝活动配置和运行数据落在两者内部。Core 源码路径由构建脚本读取 Cargo 的实际路径依赖并记录；源码在部署机器不存在时不要求恢复构建目录。修复配置由 Host `configuration` 准备，保护当前可执行文件安装目录、目标边界、配置、状态和显式 TLS 信任文件，并将 Host/Core 源码加入动作保护集。HTTP 还补充令牌和 UI 输出等控制路径；它们不能成为修复目标。

Harness 列举通过 Host RegistryBuilder 与 RemoteHarnessFactory 检查配置。普通入口不默认打开自动恢复流程；ServerConfig.recovery_config 或 start_recovery 显式启用由 Host 管理的恢复流程调度器，启动时从共享 ownership_dir 绑定 Host 的持久目标所有权。低层 open_recovery 不启动调度，由调用方负责 TargetOwnership、IncidentGuard 与关闭。独立文本修复入口仍拒绝 HumanThenHarness 规则，自动恢复流程支持 Core 的三种审核规则。

Harness 和监控复用 HostRuntime，修复会话、模拟引擎和 HTTP 回执由相应入口显式打开。自动恢复流程不因普通 Host 启动而自动派发。关闭先停止派发、请求取消并继续收集已保存的结果，再等待 Host 的在途任务结束并关闭服务；断线不证明远端执行者结束。

`demo` 与 `inspect` 继续通过 runtime 的模块生命周期框架装配封闭模拟引擎与控制台消费者。该框架用于模拟入口和嵌入模块，不替代生产 HostRuntime 的显式服务所有权与关闭顺序。

用法见[CLI](../../docs/cli.md)与[HTTP](../../docs/api/http.md)，规则见 [AGENTS](AGENTS.md)。
