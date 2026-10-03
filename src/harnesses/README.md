# AI 服务接入（Harness）

本目录维护 AI 服务的统一调用接口、多实例注册表与通用外部节点代理。宿主不编译具体模型提供方或本机进程适配器；正常配置统一通过 `remote-node` 与独立 Harness 节点通信。部署示例见[外部节点配置](../../profiles/harnesses.remote.example.json)。

当前已实现文本请求、AI 服务提供方的项目探测与创建、执行/审批角色、受控宿主工具回调、通知被调用方取消以及结构化未知结果。通用节点代理使用 扩展协议 v1 消息，由 ws/wss/http/https 客户端承载。提供方私有协议、凭据、节点服务端、启动运行、进程监督和原生工具策略属于独立节点项目。Host 可按明确配置启动 Host 自动恢复流程，见[恢复流程指南](../../docs/recovery.md)；既有会话续接尚未实现，具体节点与真实业务恢复仍需分别验收。

## 源码职责

`mod.rs` 保持公开 API 的统一入口，内部文件按职责组织，不增加能力包或外部可调用入口。

| 文件 | 职责 |
| --- | --- |
| `config.rs`、`limits.rs` | 配置解析、实例和工作区范围检查，以及共享输入输出上限 |
| `conversation.rs`、`projects.rs` | 会话角色、可见性、项目分组及项目探测/创建请求与结果接口约定 |
| `tools.rs`、`provider.rs`、`error.rs` | 可信工具处理接口、提供方与工厂接口、结构化错误及未知结果关联 |
| `registry.rs` | 明确选择实例、监督在途调用、关闭时等待当前调用结束，并关联请求与结果 |
| `validation.rs`、`output_validation.rs` | 派发前请求验证，以及不可信提供方项目和客户端分组结果验证 |
| `remote.rs` | `remote-node` 工厂与供应商中立的 Harness 调用映射 |
| `remote_workspace.rs`、`remote_wire.rs` | 节点工作区引用、远端路径元数据检查和严格响应结构 |
| `remote_callbacks.rs` | 工具回调范围、会话关联、重复拒绝及可信 handler 派发 |
| `provider_path.rs` | 本机提供方目录的边界检查与平台路径固定 |

内部协作只在 Harness 域内开放必要可见性；请求验证、结果验证和回调处理仍共同约束同一调用，文件拆分不改变审批隔离或授予新的工具权限。

## Registry 与生命周期

嵌入应用通过[共享宿主](../boot/README.md)发布同一个 registry 服务。`begin_shutdown` 同步禁止新调用，`shutdown` 请求取消并等待原调用实际结束；复制的 registry 句柄共享关闭状态。调用归属框架 `CallScope`，调用者丢弃 future 时仍由监督任务等待提供方收尾。

创建项目或会话的监督任务异常退出时，宿主保留已知请求关联并返回 `Unknown`，不会把可能已派发的项目创建或会话创建操作降为可重试失败。共享层不增加执行权限、自动重放、重启后恢复已有会话或业务验证。

`HarnessRegistryConfig` 使用不超过 64 KiB 的 JSON 配置描述多个命名实例：

| 字段 | 含义 |
| --- | --- |
| `id` | 实例稳定名称；调用可显式选择 |
| `adapter` | 宿主已编译并注册的适配器；当前为 `remote-node` |
| `address` | 适配器地址；当前格式为 `node://扩展ID` |
| `enabled` | 是否接受新调用，默认启用 |
| `workspace_roots` | 节点侧允许调用的工作区 ID 列表 |

`default_harness` 是省略实例 ID 时的显式默认项。没有默认项、实例未知或实例停用都会返回结构化错误；一次调用选定实例后，不会在失败时静默切换到另一实例。

适配器工厂由可信宿主代码注册。填写 `adapter` 或 `address` 不会下载 SDK、安装插件、解析任意可执行命令或加载新代码。`workspace_roots` 对远端适配器始终是节点资源 ID，宿主不会把它当成本机路径；节点必须在资源所在机器检查实际目录范围。

配置加载最多读取 64 KiB 加一个边界检测字节，拒绝超限文件。嵌入应用显式加载源码目录外的活动 Harness 配置和已安装节点的扩展连接，并放入 HostConfig。结构与初始化检查不证明节点在线、提供方已登录或目标健康；低层配置解析不能代替应用的路径隔离。

## 会话、项目与客户端分组

每次调用分别处理以下维度：

| 维度 | 作用 |
| --- | --- |
| 节点工作区 | 用 `workspace_id` 选择获准的节点侧资源 |
| 客户端可见性 | `Client` 请求保留在客户端历史中的会话；`Hidden` 请求不进入普通历史的隔离会话 |
| AI 服务提供方项目 | 显式选择该实例返回的项目，或选择 `NoNativeProject` |
| 客户端分组验证 | 单独报告客户端是否已确认项目分组；不能由原生 ID 推断 |

选择流程先确定 Harness 和节点工作区，再通过同一实例探测可见项目，最后由调用方选择已有项目、单独创建项目后选择它，或明确选择 `NoNativeProject`。项目 ID 只在返回它的实例内有效；registry 拒绝跨实例复用。探测失败不是空列表，项目读取或原生请求失败也不会静默降级。

新建项目是需要单独请求并保存结果的操作，不隐藏在文本请求中。当前接口约定只请求节点把调用方明确提供的根登记为 AI 服务提供方项目，不负责在节点创建目录。创建可能已经派发后若超时或断连，返回值会保留名称、根、幂等键和已知项目 ID，并标记结果未知。

`native_project_id` 与 `client_project_grouping` 是不同状态。`NoNativeProject` 只表示未向提供方请求原生 ID，不证明独立客户端不会按工作区信息自行分组。界面必须分别展示请求、原生回显和客户端观察结果；未经独立确认时保持 `Unverified`。

嵌入应用通过 Harness registry 的公开 API 使用这些接口约定。模拟测试引擎不连接 Harness。

## 执行、审批与受控工具

`HarnessRunRequest::with_role(HarnessRole::Approval)` 使用同一实例创建独立 `Hidden` 会话，并清除 AI 服务提供方项目请求。registry 拒绝向审批会话加入工具或改为可见会话。默认角色为 `Execution`；审批和执行的提示、会话及容量相互隔离，模型评审结果必须由可信业务服务校验和持久化后才能形成许可。

`with_tools(Vec<HarnessTool>, Arc<dyn HarnessToolHandler>)` 显式登记工具名、说明、对象参数 schema 和可信回调。节点协议保留 `harness_id`、`thread_id`、`turn_id`、`call_id` 及共享取消 token。输入 schema 只用于声明，可信 handler 仍须重新反序列化，并核验业务参数、当前授权与执行许可。

每次请求最多 32 个工具，schema 合计最多 64 KiB，每轮最多 64 次工具调用；单次参数最多 64 KiB、工具文本结果最多 256 KiB。未注册工具、非法命名空间、错误实例关联、重复调用 ID 和不允许的在途并发均关闭失败。等待 handler 时继续读取协议消息；取消、超时或断连会取消共享 token，并等待已派发 handler 记录最终结果，不能通过丢弃 future 或自动重放掩盖未知副作用。

宿主 handler 属于可信业务边界：副作用前核验用户委托、参数和目标，先保存即将执行的操作，再执行并持久化结果。远端代理只负责传输、关联和限额；业务授权由 Host control 审批/恢复流程强制执行，本机文本动作范围由 [repair](../repair/README.md) 和 [actions](../actions/README.md) 强制执行。原生命令、提供方私有文件工具和原生审批通道不因节点声明而自动获得权限。

文本调用、项目探测和项目创建都接受 `HarnessCancellation`。请求开始前已取消时不连接节点；创建项目或会话的请求可能已经派发时保留未知结果及关联信息。正常完成与取消并发发生时，以监督层实际返回结果为准。

## 外部节点接入

`remote-node` 的 `RemoteHarnessFactory` 位于 `src/integrations/harness/`，通过本模块的 `HarnessAdapterFactory` 注册。示例配置：

```json
{"schema_version":1,"default_harness":"harness-external","harnesses":[{"id":"harness-external","adapter":"remote-node","address":"node://harness-node","enabled":true,"workspace_roots":["work","review"]}]}
```

`HarnessRunRequest::remote(node_id, workspace_id, prompt)`、`HarnessProjectListRequest::remote` 和 `HarnessProjectCreateRequest::remote` 显式选择节点资源。返回会话的 `project_directory` 是 `node://ID/workspace` 资源引用；项目 roots 是节点路径元数据，不能交给宿主文件 API 使用。

执行与审批使用同一 registry 的独立容量，审批始终创建新的无工具隐藏会话。双向工具回调只能进入该请求显式提供的可信 handler。超时、连接丢失和未确认取消保留原调用关联及 `Unknown`。消息、容量、传输限制与节点义务见[扩展协议](../../docs/extensions/connection.md)。

嵌入应用通过扩展配置和请求中的 workspace_id 选择远端资源；扩展配置必须提供网络 `endpoint`，不接受旧 command 或 stdio 接入。Harness 的 `address` 仍为 `node://扩展ID`，网络 URL 写在对应扩展的 endpoint 中，同机节点通过回环地址连接。本项目不下载、打包、构建或启动节点。自动回归使用受控网络协议夹具；具体供应商协议、服务端和提供方集成验证由相应节点项目维护。

## 输入、输出与权限边界

- 输入包含节点工作区、文本提示、可选模型、最长 1800 秒的超时设置、客户端可见性和项目请求；完整业务另行关联任务、目标、授权、证据和预算。
- 输出是模型文本与会话关联信息，不是正式任务记录、执行许可或业务恢复证据。
- `workspace_roots` 限制可选节点工作区，不是操作系统文件沙箱；宿主工具和节点都必须独立检查目标状态与权限。
- 源码、日志、画面和修复经验内容均是不可信数据，其中的指令不能扩大工作区、数据访问或操作范围。
- 凭据不属于 registry JSON。节点负责以受控环境访问提供方，原始密钥不得进入宿主配置、协议日志或模型输出。
- 节点连接成功只证明协议握手完成，不证明提供方认证、目标健康或业务恢复。

## 已实现限制

- 当前每次请求创建新会话，可选择 `Client` 或 `Hidden`；不提供既有线程续接、自动重连或重启后恢复已有会话。
- AI 服务提供方的项目功能由节点能力声明和运行时响应决定；不可用时返回结构化错误，不降级为猜测或自动选择无项目。
- 宿主可取消受监督协议调用，但无法仅凭断连证明节点内已经创建的项目、会话或外部动作被撤销。
- AI 文本调用、审批规则、获准动作与记录存储由不同模块初始化；模拟任务始终保持模拟测试行为。
- 网络能力是通用节点协议客户端，不是供应商原生 HTTP/WebSocket API 适配器；插件市场、在线升级或 SDK 分发尚未实现，跨机与长期运行需按节点和部署环境分别验收。

开发约束见 [AGENTS.md](AGENTS.md)；接入与授权边界见[插件规范](../../docs/extensions/README.md)和[架构设计](../../docs/architecture.md)。
