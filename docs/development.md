# Host 开发指南

本指南说明从源码构建、选择检查和维护文档的顺序。开发规则见 [项目 AGENTS](../AGENTS.md)，应用设计见 [架构](architecture.md)。

## 准备环境

1. 使用 [rust-toolchain.toml](../rust-toolchain.toml)指定的 Rust 1.98.1，包含 rustfmt 与 Clippy。依赖和测试目标以 [Cargo.toml](../Cargo.toml)为准；构建环境必须提供其中声明的 Core 路径依赖。
2. 将 `CARGO_TARGET_DIR` 设置为源码外的专用绝对构建路径，将 `RECUVORA_TEST_TEMP` 设置为源码外的约定测试根。两者不能落在 Host 或本地依赖源码内，也不能经链接写入这些目录。
3. 活动配置、状态与凭据单独放在源码外。开发检查使用隔离节点替身，不需要实际提供方账号或活动部署。

Cargo 构建需要 Rust 工具链；[check.rs](../scripts/windows/check.rs)由现有 Cargo 包编译运行。Host 构建与测试均不需要 Node.js。

## 构建与检查

从项目根运行构建：

```powershell
cargo build --locked --release
```

确认两个环境变量已设置后，运行完整 Host 检查：

```powershell
cargo run --locked --features dev-check --bin recuvora-host-check -- --build-dir $env:CARGO_TARGET_DIR --test-temp $env:RECUVORA_TEST_TEMP
```

脚本统一维护在 `scripts/windows/`，通过 `dev-check` feature 启用开发工具；参数、输出路径保护和子进程环境设置见 [脚本说明](../scripts/windows/README.md)。它编译 Core 生产依赖，但不运行 Core 自身测试套件。启动 Cargo 前须设置外部构建目录，脚本参数不保护自身的首次编译输出。

只修改某个能力时，可先运行 manifest 登记的相关测试，例如修改协议消息或 schema：

```powershell
cargo test --locked --test protocol
```

实现改动完成后运行相关格式、编译、静态检查和测试，按影响范围选择完整检查。纯文档整理核对链接、锚点、命令与字段即可。不要以未运行的检查、过去的测试数量或隔离替身作为本次交付证据。

测试范围、故障注入及 `ui_contract.rs` 的默认检查和可选外部 UI 资产检查见 [测试说明](../tests/README.md)。测试只清理本次记录的临时资源，不清空共享测试根或活动部署。

## 修改与交付

1. 先读 [源码导航](../src/README.md)和目标目录的 README/AGENTS，确认逻辑归属 Host、Core、节点或客户端。
2. 修改所属模块及集中测试；涉及网络消息、业务字段、配置或 HTTP 时，同步更新对应专题文档、兼容向量或模板。
3. 按 [文档规范](AGENTS.md)维护内容归属；根入口链接详细说明，不重复抄录接口和字段表。
4. 运行适用检查，报告变更、实际结果和仍未验证的行为；新增能力或关闭问题时更新 [实现状态](implementation-status.md)。

嵌入调用者负责认证、控制路径隔离与生命周期；直接使用库不经过 HTTP 权限检查，要求见 [Rust API](host-rust-api.md)。运行与部署配置见 [项目入口](../README.md)和 [配置说明](configuration.md)。
