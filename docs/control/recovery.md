# 恢复接入

Host 将活动故障提交给 Core RecoverySession；RecoveryScheduler 安排驱动时机，RecoveryService 在 CallScope 内调用 RecoveryEngine。具体阶段判断、审批发起、执行编排、验收结果及经验重试由 Core 决定。

Host 实现当前时间、状态查询、提交、观察、审核、目标保护、执行、验收和总结能力。能力返回可信身份与证据；网络地址、平台、语言、工作区和连接配置保留在 Host。

未知结果核实时，Host 从原执行节点分别读取 reconcile 和 verify，将绑定证据交给 Core CheckResult。HTTP 不接收客户端自填的执行结果、停止状态和证据。

总结输入由 summary_context 生成有界只读投影；发生省略时保留完整审计工作，将脚本化评估设为 undetermined 并丢弃候选。总结失败交由 Core 记录，绝不重做修复。

操作与配置见[恢复指南](../recovery.md)，提交边界见[提交与历史](commits.md)。
