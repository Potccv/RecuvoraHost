# 应用配置

本目录是 RecuvoraHost 的配置边界：读取可信部署文档、解析相对路径、拒绝链接与危险重叠，并把结果转换为Core 及 Host 服务使用的配置。

- `mod.rs` 导出配置接口；`host.rs` 定义 HostConfig 与 MonitoringHostConfig，只描述待装配服务。
- `host_config.rs` 提供 Host 入口的 Harness、扩展和独立文本修复配置加载，补充 Host/Core 源码与 TLS 信任文件保护。`boot::host` 的原配置加载路径作为兼容重导出保留。
- `harness.rs` 与 `repair.rs` 保留嵌入调用的配置加载和路径准备接口；Host 入口使用 `load_host_harness_config` 与 `load_host_repair_config` 施加完整应用边界。独立文本修复入口仍在执行前拒绝 HumanThenHarness；只读历史与人工决定可加载既有配置。
- `paths.rs` 提供修复目标路径规范化和包含关系检查；`host_paths.rs` 维护 Host/Core 源码边界、外部控制路径与模拟存储路径准备。配置不反向调用 boot 或 application。
- `recovery.rs` 加载严格的 RecoveryHostConfig，使用顶层 recovery 的 RecoveryConfig，并使用 Host persistence 的 ApprovalStoreConfig 和 KnowledgeStoreConfig；要求规范目标身份和显式共享 `ownership_dir`，解析它与 `data_dir` 并拒绝目录重叠；只校验与解析，不创建存储或启动恢复流程调度器。

这里不启动服务或执行修复。Host persistence 检查日志、锁文件、链接和容量，Core 校验恢复聚合与状态转换，Host control 维护故障台账；节点继续负责实际目标与脚本执行边界。
