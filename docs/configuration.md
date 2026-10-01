# 配置与网络节点

所有活动配置由可信部署者提供并存放于源码外；本项目profiles仅含占位模板。配置启用、接口约定登记、调用权限与业务审批互相独立，不能因为字段存在或节点连接成功便获得执行权。

## 服务配置与文件

ServerConfig使用schema_version 1，listen必须回环，operator绑定可信审计身份，permissions仅接受[HTTP说明](console-api.md)列出的已实现权限。token_file、data_dir及非空的harness_config、extensions_config、repair_config、monitors_config、recovery_config、ui_dir使用源码外绝对路径；模板占位符需替换。服务状态目录和所需配置/令牌应在启动前存在。

令牌文件保存32至256个可打印非空格ASCII字节，可带末尾换行；限制文件权限，不把秘密提交到源码、UI资产或命令参数。扩展 endpoint 令牌另由 bearer_token_env 引用环境变量，不与HTTP操作员令牌混用。

repair配置中的相对路径以该配置文件目录解析；Harness的workspace_roots是节点工作区ID。活动配置、审批上下文、状态、源码、程序安装、令牌及UI输出与修复目标隔离。Host通过构建时捕获的Core路径同时保护Host/Core源码，首次写入前校验；部署机器不存在原编译源码目录时不要求重建该目录。目标及白名单文件必须预先存在，Host配置加载器可准备文本修复状态目录。

## 节点连接地址（endpoint）

每个扩展必须有endpoint，支持ws/wss/http/https；可选bearer_token_env为环境变量名，ca_certificate为额外信任的PEM CA文件，相对路径以扩展配置目录解析。URL禁止内嵌凭据、query和fragment。TLS使用既有信任根及显式CA；ws/http是明文，由可信部署选择适用范围。

kind为node或plugin，id绑定握手身份，allow_calls列出准确接口约定/版本/方法。plugin还可登记获准命名空间并通过allow_nodes使用指定节点的只读方法，不能占用保留域。接口约定或schema登记不产生写权限。

网络端点必须实现 扩展协议 v1：WebSocket文本消息各承载一个业务JSON；HTTP在单一URL上以POST Hello取得Ready及x-recuvora-session，后续POST发送消息、GET读取Result/Error/回调，DELETE释放会话。HTTP 202/204仅确认传输，不代表业务完成。每次探测/顶层调用独立会话，无自动重试、重连、重定向或线程续接。

这是与旧command配置不兼容的接入方式。旧stdio或SSH stdio节点不会被自动转换，command字段会被拒绝；须先由独立项目运行兼容网络节点，再显式填写endpoint。Host不提供节点网络服务端，不启动程序或监督节点后代。释放连接不证明远端执行者停止，已派发的不确定副作用仍为Unknown。

## 外部UI

ui_dir为null时只提供API，页面路由返回404。设置后目录必须包含固定14份官方静态文件：index.html、styles.css、data.js、shell.js、app.js、api.js、dom.js、history.js、monitoring.js、plugin-monitoring.js、project-logs.js、refresh.js、favicon.svg和favicon.ico。

启动时验证外部普通目录/文件并加载内存快照，拒绝链接与reparse point，单文件最多2 MiB、总计8 MiB。只向页面提供这些固定文件，不提供其它文件、任意路径或目录浏览；没有manifest签名验证和热更新。更新UI需更换可信产物并重启，UI目录受修复保护。

独立站点或桌面客户端使用准确allowed_origins；同源页面无需额外跨来源条目。常见桌面来源由实际运行环境确定，不能使用通配符或把来源许可当身份认证。Host不构建UI也不包含Tauri。

## 恢复流程配置

recovery_config 缺省为 null，显式填写才启动 Core 恢复流程和恢复流程调度器。服务须同时初始化 Harness、扩展和监控；缺少依赖或触发绑定不匹配时拒绝启动。恢复流程文件最多 64 KiB，字段严格且未知字段拒绝；读取配置不创建状态或派发工作。

RecoveryHostConfig 包含 schema_version:1、data_dir、必填 ownership_dir、recovery、triggers、interval_ms，以及可选 approval_store、knowledge_store。内层恢复配置由 RecoveryConfig 定义；审批和 knowledge 存储使用 Host 配置类型；领域集合限额交给 Core，文件字节限额由 Host 校验。data_dir 与 ownership_dir 可相对恢复流程配置目录解析，必须位于源码外，彼此不重叠，且与控制文件、TLS 信任文件、服务状态和旧文本修复状态分开。同一 target 不允许同时由自动恢复流程和旧文本流程管理恢复。Host 检查日志/锁文件身份及容量，并保护恢复流程配置、状态和共享所有权，旧文本动作不能改写它们。

ownership_dir 是所有保护同一规范目标的恢复存储共用的稳定权威目录，不能根据 data_dir 自动生成，也不能通过更换它绕过未完成任务。已有恢复配置须显式补充该字段；缺失时拒绝加载，不启用自动恢复。target_id 须满足 Host CanonicalTarget 的小写稳定逻辑身份规则，目标别名由可信部署者统一映射。Host 使用 Host FileTargetOwnership 取得租约；非终态与 Unknown 保留持久所有者，同一存储可以重启恢复，其他存储继续被拒绝。读取配置不会创建这两个目录。

triggers 为 1–64 项可信固定绑定，包含 monitor_id、rule_id、fingerprint、keywords、conditions，不能由观察提供方或请求正文选择；当前 rule_id 与 monitor_id 一致。interval_ms 为 10–3600000。每个触发的已登记监控必须属于 recovery.target.target_id。节点中的 executor_id、Harness address 和工作区由 Host 配置解析，不进入 Core 路由逻辑。

模板见 [恢复流程](../profiles/repair.recovery.example.json)和[服务组合](../profiles/server.recovery.example.json)。原始日志上传及 CoreSettings 不属于当前 Core 或 Host 接口；log_sources 仅用于只读查询监控目标的记录，每页最多 32 条。

旧文本修复入口支持 human 与 harness，拒绝需要定时转交的 human_then_harness；查看与管理已有记录仍可使用。显式自动恢复流程支持三种 Core 审批规则，到期审核由恢复流程调度器调度，不降级或隐式放行。

Core 0.2 的内层 `recovery` 不再包含 `max_journal_bytes`，旧字段会被拒绝，模板已移除。Host 恢复事件日志使用 256 MiB 固定上限；审批和知识日志的字节上限仍分别由 `approval_store.max_journal_bytes` 和 `knowledge_store.max_journal_bytes` 配置。已有日志的可信配置不能通过直接修改配置文件变更。旧格式不会在启动时自动迁移，导入边界见 [持久化说明](../src/persistence/README.md)。
