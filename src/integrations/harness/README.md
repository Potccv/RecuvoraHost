# 远端 Harness 适配

本目录把 Host 扩展协议的 `recuvora.harness` v1 调用映射到 Host `harnesses` 接口约定。`remote.rs` 负责 provider，`callbacks.rs` 负责有输入大小与调用次数限制的工具回调。节点地址与 workspace 只在 Host 解析，不进入 Core。
