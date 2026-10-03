# CLI 使用

所有命令由 `recuvora-host` 提供。开发时可用 `cargo run --locked --` 替代程序名，先设置源码外的 CARGO_TARGET_DIR；活动配置和运行数据也必须在源码外。

## Harness

```text
recuvora-host harness list --config PATH --extensions PATH
recuvora-host harness projects --config PATH --extensions PATH --workspace work
recuvora-host harness create-project --config PATH --extensions PATH --workspace work --name target-project --idempotency-key KEY
recuvora-host harness run --config PATH --extensions PATH --workspace work --visibility hidden --no-project --prompt "Reply with a short status"
```

PATH与KEY均为需替换的占位值。可用`--harness ID`显式选择实例，否则只使用配置中的default_harness。正常配置是remote-node；节点工作区由节点解释，不能把远端路径交给宿主文件API。网络endpoint必须已经运行且兼容 扩展协议 v1，宿主不启动节点。

run必须明确选择`--visibility client|hidden`和`--project ID|--no-project`；Hidden不能指定AI 服务提供方的项目。指定项目时先由同一实例探测确认，不存在或探测失败不降级。AI 服务提供方的项目与独立客户端分组分别报告，未独立确认时保持unverified。

文本用`--prompt`或`--prompt-file`二选一，非空UTF-8且最多64 KiB，可选`--model`。`--timeout-secs`默认180，范围1至1800；项目探测和文本请求共享预算。list只报告配置与协议连接条件，不证明 AI 服务提供方已登录或目标健康。create-project是独立会保存状态的操作，关联键不意味着节点一定具备外部去重能力。

结果为JSON，错误保留实例、会话/项目等已知关联与auto_retry=false。参数错误退出2，配置/调用失败退出1，可能已派发但无法确认的结果退出3并标为unknown，确定取消且无未知已保存的结果退出130。Ctrl-C请求取消后继续等待收尾，不能因未收到回执便自动重发。

## 审批与文本修复

```text
recuvora-host repair run --config PATH --task repair-001 --prompt "Apply the explicitly requested text change"
recuvora-host repair inspect --config PATH
recuvora-host repair inspect --config PATH --request ID
recuvora-host repair approve --config PATH --request ID --reason "Reviewed the exact change"
recuvora-host repair deny --config PATH --request ID --reason "Outside the delegated request"
recuvora-host repair revoke --config PATH --request ID --reason "Permission withdrawn"
recuvora-host repair apply --config PATH --request ID
recuvora-host repair check-result --config PATH --request ID
```

run使用可信repair配置中的目标、白名单、执行Harness及审批规则。执行和审批可以选择同一实例，但会话、上下文和容量隔离；审批为Hidden无工具。人工和Harness均不能越过审批规则限定的范围。配置字段见[profiles](../profiles/README.md)。

inspect读取完整记录，approve只保存决定，apply显式复核并执行与批准内容一致的操作。`check-result` 只核验当前内容，不重复写入；操作员需先确认旧执行者已停止。原文、替换、规则或目标变化需要新请求，Unknown继续阻断同目标冲突动作，不能换状态目录绕过历史。

当前仅支持Windows本机既有UTF-8普通文件，每文件最多16 KiB，最多64个准确相对路径。没有任意shell、创建/删除、发布或桌面操作；内容读回结果输出business_verified=false。成功/完成退出0，失败/阻断1，参数错误2，Unknown 3，待人工4，取消130。查看或批准成功不表示执行成功。

本机管理命令依赖OS用户和状态文件权限；HTTP管理使用服务令牌身份，二者不是同一身份认证机制。进程持有状态写锁，应先等待正在处理的调用结束，再由另一进程接管同一状态。

## 模拟测试

```powershell
cargo run --locked -- demo --data-dir $env:RECUVORA_DATA_DIR
cargo run --locked -- demo --data-dir $env:RECUVORA_DATA_DIR --concurrency 1
cargo run --locked -- inspect --data-dir $env:RECUVORA_DATA_DIR --task $env:RECUVORA_TASK_ID
```

data-dir 指定源码外的记录保存目录，task ID 使用 demo 实际输出。demo 运行固定模拟场景，覆盖成功、失败、验证失败、未知、超时、拒绝和取消等状态；不操作真实目标，也不调用 Harness 或网络节点。记录保留供 inspect 读取，不自动删除；模拟授权不能用于真实动作。

HTTP启动用法另见[HTTP说明](api/http.md)。`recuvora-host serve --config PATH` 在服务配置明确指定 `recovery_config` 时初始化 Host 自动恢复流程；任务查询、人工决定、恢复与核实未知执行结果使用 HTTP 管理接口，当前没有独立的 recovery CLI 管理子命令。见[恢复流程指南](recovery.md)。所有入口均保留关闭错误和未知结果，不把退出码0泛化为业务恢复。
