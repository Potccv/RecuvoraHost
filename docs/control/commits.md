# 提交与历史

## 持久提交与历史恢复

自动恢复使用 Core RecoverySession 聚合，Host 在 root lock 保护下调用 prepare，原子比较 expected_revision，将完整 SessionEntry 和 CommitRequest 写入 recovery.jsonl 并 sync_all；确认回执后安装状态并释放效果。写入失败或确认未知使写者停止，不安装推测状态。

日志头绑定 recovery-session-v1、完整 SessionConfig、存储限额及后端执行器配置。旧格式和配置不一致拒绝打开；不提供旧历史兼容或迁移。审批和知识的逻辑集合限额传入 Core；自动恢复的单一日志上限取 256 MiB、approval_store.max_journal_bytes 与 knowledge_store.max_journal_bytes 的最小值，全部用于同一聚合历史。

重开验证文件身份、头、提交请求和条目一致性，然后使用 RecoverySession::restore 并持久提交 Recover。恢复不派发效果。损坏的完整记录保留并报错，不裁剪领域安全事实。

## 增量绑定与历史导出

每条聚合提交绑定完整命令、显式时间、配置、前序摘要和子域变化。Host 只保存 latest_entry，不在每次追加复制全部历史。摘要验证不能替代文件保护；历史需完整保存，不支持删改后重新编号继续运行。

故障 IncidentLedger 使用自己的 IncidentStore，故障观察与监控检查点原子提交。独立文本修复 ApprovalStore 和独立 KnowledgeStore 仍各自保存 Core 提案，不属于自动恢复聚合。

## 跨域提交顺序

自动恢复相关领域变化已统一为一次聚合提交：创建操作与审批关联、审批消费与任务授权、执行结果与审批完成、独立核实与任务结论、经验保存与交付完成。Host 不再维护这些领域之间的中断补偿步骤。

实际外部动作无法和本机文件提交组成事务。dispatch.jsonl 在授权前记录完整操作的 prepared 边界，在进入后端前可靠记录 dispatching 边界。只有 prepared 而没有 dispatching 的可靠历史能证明本实例未派发；Host 用独立 NotExecuted 证据交由 Core 核实。存在 dispatching 的中断不自动重发。

具体动作先通过 Core PrepareAction 提交；实际网络发送前重新核对故障、所有权、提交可用性、审批期限和动作隔离。业务许可确认与外部执行之间仍可能中断，结果不确定时保留 Unknown。

## 共享目标所有权

Host 在当前故障复核、授权提交和派发之间持有目标所有权与 IncidentDispatchLease。目标身份一致、稳定 ownership_dir 和实际发送前复核共同防止更换目录或陈旧快照绕过保护。

关闭先拒绝新工作并排空调用，再同步聚合与派发日志；只有 Core 判断可释放时才解除持久归属，最后关闭句柄。失败保留句柄并允许重试。取消、断连和文件锁释放均不证明远端执行者已停止。

## 经验总结、交付与隔离

失败和 Unknown 的实际动作隔离随结果进入聚合历史。总结失败、容量不足或交付重试不删除隔离，也不重新执行修复。Core 生成稳定交付身份；Host 原子保存经验及交付完成状态。物理验证范围见[实现状态](../status.md)。
