# 命令行接口（CLI）

本目录负责严格参数解析、Host Harness 使用与命令输出；实例、目标、审批规则和服务生命周期由 boot 初始化，恢复授权使用 Host control 接口。只接受明确命令及有大小限制的输入，不从模型文本解析终端命令。

Harness 支持 list、projects、create-project 与 run，正常实例统一为 remote-node；节点工作区是资源 ID，不是宿主路径。可见性与 AI 服务提供方项目选择显式选择，项目必须由同一实例探测，失败不改派或降级。

repair 支持 run、inspect、approve、deny、revoke、apply 与 `check-result`。人工批准只记录决定，apply 再次核验并使用执行许可；`check-result` 读取状态，不重复写入。输出保留关联 ID、Unknown、auto_retry=false 和业务未验证语义。

命令语法与退出码见[CLI说明](../../docs/cli.md)，身份和约束见 [AGENTS](AGENTS.md)。
