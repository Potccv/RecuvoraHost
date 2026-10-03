# 扩展运行时

本目录实现以下宿主基础设施：

- NetworkEndpoint 与可信配置解析。
- HTTP/WebSocket/TLS 会话和 Bearer 注入。
- ExtensionClient 与请求监督。
- ExtensionRegistry、接口归属、实例状态与关闭。
- 插件只读 service.call 回调和调用容量。
- 通用、无 UI 语义的隔离 descriptor 调用。
- `ui_links.rs` 维护可选页面能力、可信地址绑定、专用只读描述调用及运行实例快照；`page_urls.rs` 校验基础目录、来源、编码与相对导航地址。

页面默认关闭，只有明确启用、正确登记且获准的插件才在连接装配完成后读取描述。`ui_links`/`ui_links_catalog` 读取快照，`refresh_ui_links` 显式刷新；每插件一个在途调用，固定 5 秒期限，不提供回调，不自动轮询或重试。失败撤下链接，旧 revision 只供诊断；关闭先阻止发布和新派发，再取消等待已有调用。配置绑定不可在运行中变更，重启建立新实例。页面契约与 HTTP 消费见[页面说明](../../../docs/extensions/pages.md)，业务许可仍由原可信路径管理。

Host wire/schema 统一在本包 `src/protocol/`；监控、Harness 和修复的业务解释分别由相邻适配目录完成。

监督任务保留可信回调的 rejected/cancelled/unknown 分类；回调 Unknown 在顶层成功和取消收尾后仍然返回，不能重放。派发后的协议、连接和清理失败按 Unknown 处理，并等待可信回调结束后释放容量。Harness 保留方法按登记的输入/输出 schema 校验，有副作用结果不符保持 Unknown，详见[节点接口](../../../docs/extensions/nodes.md)。

内部脚本执行路由必须携带可信 `DispatchGuard`。客户端在完成握手和身份核对后、发送顶层 Call 前重新验证故障、所有权、审批与知识条件；门一直持有到发送完成或失败，然后立即释放。派发前复核失败为拒绝且没有节点调用，发送失败保持 Unknown。所有提前退出和监督任务展开路径均释放门；该门不授予许可，也不延长审批或观察的有效时间。

通用 DispatchGuard 仅定义 validate/release，不依赖恢复动作类型。恢复会话通过 [RepairActionGuard](../../recovery/contract.rs) 先持久准备具体动作，再把发送门交给本目录；动作准备不属于协议客户端的职责。
