# Windows 脚本规范

继承 [脚本规范](../AGENTS.md)和 [项目规范](../../AGENTS.md)。

- Windows 开发检查脚本统一放在本目录，以 Rust 实现并由根 Cargo 包登记；不创建第二个 Cargo 包或 PowerShell 包装脚本。
- 检查使用子进程局部环境和工作目录，不修改调用方 PATH、工具链、凭据或全局环境。
- 路径保护同时拒绝 Windows reparse point 与符号链接；保持源码外输出、失败即停止和限定清理范围。
- 回归测试集中在项目 `tests/`；修改命令或参数时同步维护本目录 README 和相关使用入口。
