# HTTP API v1

`recuvora-host serve`提供带 Bearer 认证的 Host 应用接口。HTTP与CLI直接编译，不需要server或web-ui feature；本应用不嵌入页面，可选择托管外部ui_dir。UI是否连接不影响后台服务生命周期。

## 启动与身份

```powershell
cargo run --locked -- serve --config $env:RECUVORA_SERVER_CONFIG
```

ServerConfig的schema_version为1，listen只接受回环地址。token_file、data_dir及非空能力配置/UI路径必须是源码外绝对路径；状态目录、令牌及所需文件在启动前准备。最小占位模板见[server profile](../../profiles/server.example.json)，字段解释见[配置说明](../configuration.md)。服务配置本身也放源码外。

令牌为独立随机生成的32至256字节可打印非空格ASCII秘密，文件可带末尾换行。请求使用`Authorization: Bearer ...`，operator从服务配置绑定，不能由请求正文指定。当前为单操作员模式，没有多用户账户或OAuth。令牌文件不进入源码、UI构建或日志。

API请求验证准确Origin：同源允许，跨来源必须匹配allowed_origins，不接受通配符。OPTIONS预检不执行业务；来源通过后业务请求仍需Bearer。跨机访问由部署方提供TLS代理或受控转发，Host本身不直接绑定公网。

可配置权限为 harness.run、harness.projects、repair.run、approval.decide、approval.apply、approval.check_result、simulation.run、logs.read、operation.cancel、extension.read、monitor.read、incident.read、incident.acknowledge、recovery.read、recovery.decide、recovery.resume、recovery.check_result、knowledge.read。未知权限拒绝启动，按需要缩减；节点调用白名单和实际审批许可还会独立核验。恢复流程自动调度由可信配置启用，HTTP 权限只控制操作员访问和人工管理，不替代恢复流程内的审批规则与执行许可。

## 静态客户端

ui_dir为空时页面路由返回404。配置后启动加载固定14份官方静态文件为内存快照，只提供这些名称；未知名称、路径穿越或目录浏览不被提供。单文件2 MiB、总计8 MiB，拒绝链接/reparse point。目录及文件是可信部署输入，没有安装包/manifest签名验证，不向网页提供目录内其它文件。

页面及静态文件不需要API令牌，以便展示认证页；`/api/v1`仍经过认证。页面更新需重启服务，服务不会自行构建、更新或安装UI。ui_dir加入修复保护范围。独立Web/Desktop也可以不配置ui_dir而直接连接API，需按实际来源配置CORS。

## 资源

下表路径均以`/api/v1`为前缀。JSON请求正文上限128 KiB；错误响应含error.code、error.message和auto_retry:false。

| 方法与路径 | 内容 |
| --- | --- |
| `GET /bootstrap` | schema_version 1的runtime、permissions、capabilities、harnesses、配置、监控及历史首批摘要 |
| `GET /repairs`、`/approvals`、`/operations`、`/simulations` | 摘要分页与筛选，items、next_cursor、total、limit、order |
| `GET /repairs/{id}`、`/approvals/{id}` | 单条完整回复、操作、证据与规则 |
| `GET /harness/{id}/projects` | workspace_id对应的 AI 服务提供方项目，失败不冒充空列表 |
| `POST /harness/{id}/runs` | operation_id、workspace_id、prompt、visibility、project_id、model、timeout_secs |
| `POST /repairs/runs` | operation_id、task_id、prompt；可选成对incident_id与incident_revision |
| `POST /approvals/{id}/{approve,deny,revoke,apply,check_result}` | revision、reason；批准、执行及只读结果核实分别授权 |
| `POST /simulations` | operation_id、task_id、target、scenario、timeout_ms，仅模拟测试 |
| `GET /operations/{id}` | 已接受请求状态与结果 |
| `POST /operations/{id}/cancel` | 请求取消，仍需读取最终状态 |
| `GET /logs` | level、source、task_id、operation_id、query、cursor、limit |
| `POST /extensions/{id}/query` | contract、version、method、params，调用获准的只读接口 |
| `GET /monitors`、`/monitors/{id}` | 监控与发现摘要、详情；要求monitor.read |
| `GET /monitoring/plugins/{id}` | 宿主通用监控和受限插件描述；专属读取额外要求extension.read |
| `GET /ui/catalog` | 要求 extension.read；返回 views、external_links、link_statuses，views 额外按 monitor.read 过滤 |
| `GET /ui/plugins/{plugin_id}/views/{view_id}` | Host 校验并包装的只读 view 文档；当前实现 `monitoring_v1` |
| `POST /ui/plugins/{plugin_id}/links/refresh` | `{}`；要求 extension.read；显式刷新插件页面描述，返回快照及 auto_retry:false |
| `GET /monitors/{id}/logs` | Host 已持久接收的 Node 错误回执；要求monitor.read、logs.read和extension.read |
| `GET /incidents`、`/incidents/{id}` | 故障摘要与完整证据；要求incident.read |
| `POST /incidents/{id}/acknowledge` | revision、note；要求incident.read与incident.acknowledge |
| `GET /recovery/status` | 恢复流程调度器的 running 与 last_error；要求 recovery.read |
| `GET /recovery/tasks` | Host 任务摘要分页；要求 recovery.read |
| `GET /recovery/tasks/{id}` | RecoveryTask 任务详情，包含故障、已提交操作、实际动作和独立证据；要求 recovery.read |
| `GET /recovery/tasks/{id}/approval` | 原始 Core ApprovalRecord，无审批时 record:null；要求 recovery.read |
| `POST /recovery/tasks/{id}/decision` | 审批 revision、decision:approve/deny/escalate、reason；要求 recovery.read 与 recovery.decide |
| `POST /recovery/tasks/{id}/resume` | 任务 revision；要求 recovery.read 与 recovery.resume |
| `POST /recovery/tasks/{id}/check_result` | operation_id、任务 revision；要求 recovery.read 与 recovery.check_result；异步读取节点证据 |
| `POST /recovery/knowledge/search` | conditions、可选 keywords、limit:1–100；只读，要求 knowledge.read |

## 自动恢复流程接口

recovery_config 显式启用恢复流程；未配置的资源返回 503，无访问权限返回 403，未知任务返回 404。`/recovery/tasks` 使用共同 cursor、limit、state、query、task_id 参数，state 保留 Core 的 snake_case 阶段；任务详情由 Core 记录生成；`result_check` 保存核实结果，执行证据中的 `checked_at_ms` 为核实时刻。审批使用完整 Core 类型。未获取完整操作和规则前不能批准。

decision 的 revision 是 ApprovalRecord.revision，resume/check_result 的 revision 是 RecoveryTask.revision。actor 由可信 operator 绑定，请求不能提供身份、验收或执行事实。过期、revision/状态冲突返回 409。JSON 结构拒绝返回 422，语义无效返回 400，存储或服务不可用返回 503；均使用共同错误格式。已返回 202 的异步节点查询失败写入 operation.error，客户端需读取操作结果。

人工决定与 resume 在成功保存到磁盘后返回记录，auto_retry:false。决定自身不执行动作；已启用的恢复流程调度器在后续轮次继续处理获准任务。resume 仅恢复 paused（暂停）任务；Unknown（未知执行结果）必须先核实。check_result 先保存稳定 operation_id，返回 202，再从绑定节点分别获取原执行状态和业务验收。客户端用 `/operations/{id}` 读取结果，任何回执未知都不自动重复 POST。节点不支持只读 reconcile 或证据不兼容时，不重复执行脚本。

修复经验搜索是只读 POST，不创建 operation。conditions 为 1–32 项准确条件，keywords 最多 32 项，limit 为 1–100；匹配规则由 Core KnowledgeQuery 决定。经验记录只通过 `experiences` 数组返回，元素为完整 `RepairExperience`，包含结果、证据、实际 `actions` 和总结报告。失败和 Unknown 仍作为明确标记的负面参考返回；按记录时间降序、ID 升序排序后应用 limit。实际动作版本隔离不会删除负面经验，结果核实也不会解除永久隔离。候选和经验均不授予执行权限。

恢复流程的 submit/advance 由可信恢复流程调度器管理：已配置目标的 Node 错误报告在持久接收后按原文与来源身份交给 Core，Host 不设置故障触发筛选策略；不开放客户端上传 ProblemContext、脚本或规则。`/repairs` 与 `/approvals` 提供独立文本修复视图，不混入自动恢复流程记录；两者状态目录分开。流程及证据见[恢复流程说明](../recovery.md)。

## 接受、结果与重复请求

异步请求先将 operation_id 保存到磁盘，再返回`202 {operation_id,auto_retry:false}`。客户端应先生成稳定关联ID，回执丢失只用GET核验；重复ID返回409且不再次派发，ID未找到也不是外部动作从未发生的通用证据。

操作状态为running、completed、failed、canceled或unknown，时间使用Unix毫秒。completed只表示服务调用结束，仍须读取result：待人工、模拟失败和文件读回都不能转换为业务恢复成功。已派发后超时、断连或进程终止保留Unknown，重启不自动重放。

HTTP应用日志与 Core 审批/故障记录分别承担传输接收和业务判定责任。请求取消不等于执行者已经停止；停止服务会请求通知被调用方取消、等待在途结果并等待相关服务完成当前任务。外部动作与回执写盘不是同一事务，不承诺精确一次。

## 分页、审批与故障

摘要默认每页25条，limit允许1至100，游标按不可变ID降序继续。初始化历史只读首批，不含完整规则、文件全文、完整模型回复或获准动作，不能据摘要批准。审批待处理队列独立于已结束历史，全局计数来自服务统计，不按当前页长度推断。

人工决定、执行及未知执行结果核实在审批存储服务的同步锁内核验 revision，冲突返回409，客户端必须重新读取完整详情。独立文本流程批准仅记录决定，apply 显式执行；自动恢复流程由显式启用的恢复流程调度器在批准后继续调度。check_result 查询证据并保存核实结果，不重做动作。Unknown目标阻断冲突执行，参数或UI状态不能绕过规则。

故障确认独立返回保存在磁盘上的记录，不创建通用执行operation。acknowledged只表示已知悉，resolved只表示异常条件解除，二者都不授权修复或证明业务恢复。确认须携带当前revision，actor由服务绑定，未知回执不可自动重复POST。

修复请求可显式关联incident_id和incident_revision，需repair.run与incident.read，并校验故障目标与固定修复目标、规则一致。接受时保存大小受限的来源快照，后续故障变化不改写历史。关联不确认/解除故障，也不会由监控自动派发修复。

## 只读视图与记录

插件监控通过通用 UI catalog/view GET 接口读取；旧 monitoring 路径保留兼容。专属描述须登记并满足权限、schema和限额，失败以独立状态降级，不加载插件HTML、脚本或资源。插件描述不能覆盖 Host 接收、授权或恢复事实。字段 source 只接受 monitor 或 last_error_log，后者指向 Node 最后一个已接收错误对象，不接受旧 last_value。监控列表与 bootstrap 不包含错误原文；单条详情和插件监控投影仅在同时具备 logs.read 时包含 last_error_log。

独立页面与声明式 view 并列提供。目录保持 schema_version 1 和 views，新增 external_links 扁平入口数组及 link_statuses 插件状态数组；缺少 monitor.read 时只过滤 views，不阻止获准的外部页面读取。目录使用登记实例内快照，不触发插件调用。页面字段、URL 绑定和状态详见[独立页面契约](../extensions/pages.md#host-向-ui-提供的导航对象)。

页面刷新只读取固定 describe_ui_links 描述，不创建执行 operation、审批或恢复事实。返回 `{schema_version:1,plugin:快照,auto_retry:false}`，快照带 links；200 中的 unavailable/invalid_response 仍表示描述失败且链接已撤下。忙为409，无法派发为503，未知插件为404；客户端根据响应状态显示诊断，不自动重试。插件网页由插件自行提供和鉴权，Host 不代理页面或传递操作员令牌。

`GET /monitors/{id}/logs` 只读取已登记接收实例对应的持久错误回执。Node 在错误批次中提供 fingerprint、message 和 evidence，Host 不按级别、事件或文本重新分类。服务不再接受 log_sources、字段映射或 error_levels/error_events 配置；GET 不调用 Node、不推进 Node 读取检查点、不创建 Core 任务。日志展示与恢复交付共用同一份持久收件。

错误回执从最早记录开始，按不可变回执 ID 升序分页；每页 limit 为 1 至 32，总响应最多 256 KiB，大小上限可使实际页更短。items 与 errors 包含同一页错误，message 保留 Node 原文；level 固定为 ERROR 仅用于展示，event 为 Node fingerprint，node_log_id 保留 Node 身份，timestamp 是 Host 接收时间（timestamp_kind 为 received_at）。record_kind 为 node_error，full_log 固定为 false；此接口不提供普通日志或全量日志。

next_cursor 是仅供 HTTP 使用的游标，绑定接收实例、target 和 source，锚定本页最后一条回执；即使当前没有记录也返回可继续读取的游标。服务最多保留 256 个游标，闲置 30 分钟、容量淘汰、重启或锚点不可用后返回 409 cursor_invalid，客户端须显式从头读取，不静默跳过错误。has_more 表示仍有已接收回执；source_error 与 coverage 来自接收状态，Node 断连不妨碍读取既有回执，空页不表示目标健康。

接口约定检查范围见[测试说明](../../tests/README.md)。真实节点、跨机网络、浏览器/Tauri交互及业务恢复仍需部署验收。

## 统一修复经验字段

`GET /api/v1/recovery/tasks/{id}` 在原 `task` 之外返回 `experience_jobs`，仅列出该任务未交付的经验工作：`id`、`attempt`、`pending`、`summarized`、`last_error`。读取仍要求 `recovery.read`；这些字段不授予重执行权限。

`POST /api/v1/recovery/knowledge/search` 要求 `knowledge.read`，响应为 `{ "experiences": [...], "limit": 请求限额, "auto_retry": false }`，不提供 `items` 或脚本案例兼容字段。经验可以没有动作；`actions` 中的产物和报告中可选候选采用中立 `RepairArtifact`，具体脚本格式由 Host executor 解释。HTTP 不接受客户端上传结果、脚本化结论或成功断言；总结失败的额外重试目前由可信 Rust 接口提供。
