# Windows 开发检查

[check.rs](check.rs)由根 Cargo 包登记为开发工具 `recuvora-host-check`，使用已有 Cargo 执行本包检查，不安装工具链或更改全局 PATH。工具链由 [rust-toolchain.toml](../../rust-toolchain.toml)指定。

`dev-check` feature 仅启用开发工具，普通产品构建不包含该程序。检查子命令不启用此 feature，避免 Windows 在测试编译时覆盖正在运行的检查工具；脚本代码仍由集中回归测试编译和静态检查。

先将 `CARGO_TARGET_DIR` 和 `RECUVORA_TEST_TEMP` 设置为源码外的专用绝对路径，再从项目根运行：

```powershell
cargo run --locked --features dev-check --bin recuvora-host-check -- --build-dir $env:CARGO_TARGET_DIR --test-temp $env:RECUVORA_TEST_TEMP
```

首次运行由 Cargo 编译开发工具，必须在启动 Cargo 前设置外部 `CARGO_TARGET_DIR`；脚本参数只能保护随后执行的检查输出，不能撤销启动阶段已经发生的编译。该命令不需要 PowerShell 脚本执行策略；代码块仅使用当前终端的环境变量语法。

| 参数 | 要求 |
| --- | --- |
| `--build-dir PATH` | 必填，源码外绝对构建目录 |
| `--test-temp PATH` | 必填，源码外约定测试根；别名 `--temp-root` |
| `--cargo-path PATH` | 可选，已有 Cargo 程序；默认 `cargo` |
| `--help` | 单独显示帮助，不查询 metadata 或执行检查 |

脚本通过 Cargo metadata 识别本项目及直接本地路径依赖，拒绝输出位于这些源码树内或通过符号链接/reparse point 写入。两个输出路径都通过校验后才创建目录，不清空传入目录。测试临时根须符合旧文件动作的路径限制；隐藏目录仅在它恰好是配置的测试根时被豁免，不能把隐藏根下的子目录改配为测试根。

检查顺序为 `cargo fmt --package recuvora-host -- --check`、`cargo check --all-targets --locked`、`cargo test --all-targets --locked`、`cargo test --doc --locked` 和 `cargo clippy --all-targets --locked -- -D warnings`。格式检查限定本包，其它命令编译本地依赖，不运行 Core 自身测试套件。

仅检查子进程接收构建目录、测试根、四个测试线程与项目工作目录，调用方环境和目录保持原值。参数错误退出 2；metadata、目录准备或检查失败退出 1，并停止后续检查；全部成功退出 0。各测试只清理自己创建且记录的资源，不处理活动部署。

验证范围见 [测试说明](../../tests/README.md)，维护规则见 [AGENTS](AGENTS.md)。
