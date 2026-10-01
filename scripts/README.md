# 宿主开发脚本

Windows 开发检查的 Rust 实现统一维护在 [windows](windows/README.md)，由现有 Cargo 包构建，不依赖 PowerShell 脚本。

从项目根运行前，先设置源码外的绝对构建目录和约定测试根：

```powershell
cargo run --locked --features dev-check --bin recuvora-host-check -- --build-dir $env:CARGO_TARGET_DIR --test-temp $env:RECUVORA_TEST_TEMP
```

参数、输出路径保护、检查顺序与失败退出见 [Windows 检查说明](windows/README.md)。开发环境准备见 [开发指南](../docs/development.md)，验证范围见 [测试说明](../tests/README.md)，共同规则见 [AGENTS](AGENTS.md)。
