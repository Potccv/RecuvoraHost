# 远端 Harness 适配

本目录把 Host 扩展协议的 `recuvora.harness` v1 调用映射到 Host `harnesses` 接口约定。`remote.rs` 负责 provider，`callbacks.rs` 负责有输入大小与调用次数限制的工具回调。节点地址与 workspace 只在 Host 解析，不进入 Core。

工具处理器返回的不确定项目/会话结果保留为 wire Unknown；派发前 Interrupted 映射 cancelled，明确拒绝映射 rejected。回调派发后的协议、连接、超时或超限输出保持 Unknown，由扩展监督任务保留，普通会话成功文本不能覆盖它。三个 Harness 方法均执行节点声明的输入/输出 schema，具体规则见[节点接口](../../../docs/extensions/nodes.md)。
