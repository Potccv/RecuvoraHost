# 应用服务启动

本模块分派 `serve`、`harness`、`repair`、`demo` 与 `inspect`。参数展示留在应用层，恢复服务及最终状态由 `recuvora_core` 提供；节点通信通过 Host ExtensionRegistry，不管理节点进程。

Host 在任何状态目录创建或日志打开前检查 Host 与 Core 源码边界，拒绝活动配置和运行数据落在两者内部。Core 源码路径由构建脚本读取 Cargo 的实际路径依赖并记录；源码在部署机器不存在时不要求恢复构建目录。修复配置由 Host `configuration` 准备，保护当前可执行文件安装目录、目标边界、配置、状态和显式 TLS 信任文件，并将 Host/Core 源码加入动作保护集。HTTP 还补充令牌和 UI 输出等控制路径；它们不能成为修复目标。

Harness 列举通过 Host RegistryBuilder 与 RemoteHarnessFactory 检查配置。普通入口不默认打开自动恢复流程；ServerConfig.recovery_config 或 start_recovery 显式启用由 Host 管理的恢复流程调度器，启动时从共享 ownership_dir 绑定 Host 的持久目标所有权。低层 open_recovery 不启动调度，由调用方负责 TargetOwnership、IncidentGuard 与关闭。旧文本修复入口仍拒绝 HumanThenHarness 规则，自动恢复流程支持 Core 的三种审核规则。

Harness 和监控复用 HostRuntime，修复会话、模拟引擎和 HTTP 回执由相应入口显式打开。自动恢复流程不因普通 Host 启动而自动派发。关闭先停止派发、请求取消并继续收集已保存的结果，再等待 Core 的当前任务结束并关闭服务；断线不证明远端执行者结束。

用法见[CLI](../../docs/cli.md)与[HTTP](../../docs/api/http.md)，规则见 [AGENTS](AGENTS.md)。
