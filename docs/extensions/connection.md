# 扩展协议接入

## 共同协议与 Host 职责

Host 在本包 `src/protocol/` 实现消息类型、schema 校验、身份、版本、ID 规则与帧限额。外部节点/插件按[扩展协议 v1 规范](protocol.md)自行实现，不依赖 Host、Core 或共享协议包；固定消息样例见[兼容向量](examples/protocol.json)。Host 实现网络传输、TLS、向请求添加认证信息、注册、白名单、容量及业务适配；Core 不包含网络客户端。Host 统一维护对外规范与自身协议实现，不启动节点进程。

v1 采用 UTF-8 JSON，消息包含 hello、ready、call、result、error、callback、cancel。身份、角色和版本严格匹配；每次顶层调用独立握手且声明须与登记时一致。固定帧上限 1 MiB，调用上限 1800 秒；无自动重连、重放或线程续接。

## 网络传输

连接地址（endpoint）支持 ws/wss/http/https，禁止 URL 内嵌凭据、query 和 fragment。WebSocket 每条文本消息承载一个 Message；HTTP 在同一 URL 上使用 POST Hello、POST 消息、GET 响应、DELETE 释放会话，session 通过 x-recuvora-session 关联。202/204 只确认传输，不能表示执行完成。

TLS 使用既有信任根和显式 PEM CA，节点 Bearer 从可信环境变量读取。配置及字段见 [配置](../configuration.md)。旧 command、stdio 与 SSH stdio 不兼容；任意 REST 或模型 API 地址不能直接作为节点 endpoint。

## 业务接口约定

实现节点时，具体字段和载荷样例见[节点业务接口](nodes.md)。

独立插件页面的可选描述契约另见[插件独立页面约定 v1](pages.md)；它复用共同消息，接入状态见 [HOST-004](../status.md#host-004)。

| 接口约定 | Host 使用 |
| --- | --- |
| `recuvora.harness` v1 | 远端项目、会话与受控工具回调；审核使用独立容量和无工具会话 |
| `recuvora.repair` v1 | 节点 inspect/verify，以及仅使用 Core 许可后的 execute_script |
| `recuvora.repair` v1 的可选 reconcile | 只读原操作执行事实；与独立 verify 共同用于 Core 的执行结果核实 |
| 插件命名空间接口约定 | 明确获准的 Node 错误批次 v2、发现与只读查询；登记不产生业务写权限 |
| 声明式 UI view | Host 校验并过滤只读描述，不执行插件 HTML 或脚本 |

repair 方法只能由 kind: node 提供；inspect、verify、可选 reconcile 必须声明 read_only:true，execute_script 必须为 false。普通公开只读路由不能调用 execute_script。节点 reconcile 的证据结构见[恢复流程](../recovery.md)；check_result 是 Host HTTP / 服务管理入口名称，不是节点 wire 方法。未声明或未获准的方法直接失败，不自动回退。

消息关联、schema 和连接状态仅证明结构或传输条件，不证明提供方事实真实。取消和连接关闭不证明外部执行者停止；不确定副作用保持 Unknown。

Host 保留回调结果分类，Unknown 不被顶层成功覆盖；Harness 方法在派发前和接收结果后执行声明 schema 校验。具体结果规则见[扩展协议 v1](protocol.md#标识版本与结果含义)，固定回归与当前验收范围见[实现状态](../status.md)。

验证边界见[实现状态](../status.md)，初始化见[插件](README.md)。
