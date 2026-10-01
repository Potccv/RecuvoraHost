# 协议模块规范

继承[源码规范](../AGENTS.md)。

- 本模块是 Host 的唯一 wire/schema 实现，不建立共享协议 Cargo 包，也不要求节点依赖宿主。
- 对外契约以 `docs/extension-protocol-v1.md` 和版本化 JSON 样例为准；节点/插件独立实现和验证。
- 消息表示与显式校验分开；保持现有 JSON、ID、限额、Unknown 和取消语义。
- 网络、TLS、配置、注册和授权不进入本模块；不复制 Core 状态机。
- 破坏性 wire 变化提升协议主版本，并同步规范、兼容样例和集中测试。
- 测试放在项目 `tests/`；节点测试不能通过依赖 Host 来复用实现。
