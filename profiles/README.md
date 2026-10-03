# 宿主配置模板

模板只包含中立占位值，不能直接作为活动部署使用。将所需副本放到源码外，按实际路径、节点和最小权限调整；令牌另行生成并限制文件权限。

| 模板 | 内容 |
| --- | --- |
| [server.example.json](server.example.json) | 回环HTTP、令牌、状态、可选恢复能力配置和ui_dir |
| [server.recovery.example.json](server.recovery.example.json) | 显式自动恢复流程的服务组合与最小管理权限 |
| [extensions.example.json](extensions.example.json) | 网络endpoint、node身份与Harness方法白名单 |
| [extensions.network.example.json](extensions.network.example.json) | wss网络节点与令牌环境变量占位 |
| [extensions.pages.example.json](extensions.pages.example.json) | 独立插件页面能力、描述方法白名单和浏览器基础地址绑定 |
| [harnesses.remote.example.json](harnesses.remote.example.json) | remote-node实例、默认项与work/review节点工作区 |
| [repair.local.example.json](repair.local.example.json) | 明确目标/白名单、执行/审批及有限期规则 |
| [repair.recovery.example.json](repair.recovery.example.json) | schema 2 恢复流程、独立 executor 配置、审批、触发与间隔 |
| [monitors.example.json](monitors.example.json) | 抽象只读来源、规则、采样期限和时效 |

server模板中的绝对路径占位符必须替换，data_dir和token_file实际存在后才能启动。可选能力路径与ui_dir默认为null，不填演示提供方；独立客户端连接时补充准确allowed_origins。可选ui_dir指向可信外部Web构建目录，不能指向任意源码或凭据目录。

扩展模板的回环URL仅是占位节点服务，本项目不实现该服务。节点需已运行并支持扩展协议 v1网络会话；旧command、stdio或SSH stdio配置不兼容。配置bearer_token_env只写变量名，不保存令牌；需要TLS自定义CA时明确设置ca_certificate。

页面模板显式启用 ui_links，插件必须声明 recuvora.ui_links.v1 并实现白名单中的 describe_ui_links。entrypoints 是用户浏览器可达的基础地址，独立于 Host 协议 endpoint；模板域名必须按部署替换。省略 ui_links 时页面能力默认关闭；修改绑定后重启服务。目录提供入口不代表网页可达、用户已登录或已获准执行。

repair的harness_config与extensions_config引用外部活动文件，target-copy/settings.txt须预先存在；本机真实文本动作仅限Windows。执行与审批使用独立会话及节点工作区，人工批准也不能越过目标/动作白名单。状态目录不能随意更换以绕过未知记录。

monitor占位接口约定com.example.observation没有内置提供方；由兼容插件/节点提供只读观察。监控自身不自动派发修复，确认收到也不解除故障。只有显式填写 recovery_config 或调用 HostRuntime.start_recovery 才会启动恢复流程调度器，处理按配置绑定的目标故障。

恢复流程模板的外层和 recovery 均采用 schema_version 2；recovery 使用 Core 配置类型，executor 管理 platform、allowed_languages、diagnostic_queries，approval_store、knowledge_store 为 Host 存储限额配置；外层 data_dir 和必填 ownership_dir 相对活动恢复流程配置解析，彼此及与 console/独立文本修复状态分开。保护同一规范目标的所有恢复存储共用一个稳定 ownership_dir，未完成或 Unknown 不得通过更换目录绕过互斥。替换 Harness、executor 与 monitor 身份时须同时更新对应配置和准确方法白名单。恢复审批显式允许 repair_with_harness，具体动作限制为 execute_script，summary_timeout_secs 独立约束总结。节点需实现 recuvora.repair v1；核实未知执行结果额外需要只读 reconcile。模板不包含执行节点或任何实际业务恢复结论。

字段和边界见[配置说明](../docs/configuration.md)，接口见[HTTP说明](../docs/api/http.md)，开发规范见 [AGENTS](AGENTS.md)。
