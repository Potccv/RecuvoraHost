# 宿主脚本规范

继承[项目规范](../AGENTS.md)。

- Windows 开发检查的 Rust 脚本统一放在 `windows/`，通过本项目 Cargo 包的 `dev-check` feature 登记开发工具，不新增 Cargo 包或 PowerShell 包装入口。
- 输出、缓存和测试数据必须在 Host 与本地依赖源码外，拒绝非绝对路径及符号链接/reparse point。
- 不下载节点、不启动 UI 构建、不修改全局工具链、PATH 或凭据。
- 检查默认包含本包 CLI 与 HTTP；不使用不存在的 server/web-ui/desktop-ui feature。
- 保留失败退出，不将未执行检查标成通过；检查本包不等于运行 Core 自身测试。
- 子进程使用局部环境和工作目录，调用方环境保持原值。清理只处理本次记录的资源，禁止递归清空调用方目录或活动部署。
