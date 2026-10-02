# 应用配置

本目录是 RecuvoraHost 的配置边界：读取可信部署文档、解析相对路径、拒绝链接与危险重叠，并把结果转换为 RecuvoraCore 接受的领域配置。

- `mod.rs` 加载 Harness 与独立文本修复配置，并生成修复目标之外的保护路径。
- `paths.rs` 提供只在 Host 配置准备阶段使用的路径规范化和包含关系检查。
- `recovery.rs` 加载严格的 RecoveryHostConfig，使用 RecoveryConfig，并使用 Host persistence 的 ApprovalStoreConfig 和 KnowledgeStoreConfig；要求规范目标身份和显式共享 `ownership_dir`，解析它与 `data_dir` 并拒绝目录重叠；只校验与解析，不创建存储或启动恢复流程调度器。

这里不启动服务或执行修复。Host persistence 检查日志、锁文件、链接和容量，Core 校验领域历史恢复一致性；节点继续负责实际目标与脚本执行边界。
