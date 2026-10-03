# 持久适配

`journal` 提供配置与身份绑定的单写者日志、版本比较、容量限制和可靠同步。提交仅在追加及 sync_all 成功后确认，写入错误使当前实例停止写入；不确定结果通过受保护历史重开核实。

`incidents` 保存 Host IncidentLedger 的故障与检查点事务；`approval` 和 `knowledge` 是 Core 独立领域的存储适配，用于独立文本审批或明确的知识管理。自动恢复使用 RecoveryService 的单一 RecoverySession 日志，不再打开独立审批及知识日志。

序列化记录不是权威状态安装入口，重放由所属领域校验；文件锁不能证明远端动作停止。旧格式和退休代次拒绝打开且保留数据。聚合限额、派发边界与关闭见[提交与历史](../../docs/control/commits.md)。

集中测试在[tests](../../tests/README.md)：persistence 检查锁、身份、配置和损坏；persistence_faults 检查确认丢失；recovery_commits 使用实际子进程退出检查原子业务边界、无重派和经验幂等。
