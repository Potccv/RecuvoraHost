# 外部错误日志来源适配

`RegistryObservationSource` 将 ExtensionRegistry 的只读调用转换为 Host ObservationSource。poll 必须绑定已登记且 kind 为 Node 的来源；Plugin 可以登记业务契约及提供发现入口，但不能直接提供错误事实。响应使用 monitoring 的 schema v2 错误批次，发现清单保持 schema v1。

Host monitoring 校验身份、限额、来源时效、覆盖与游标，并原子持久接收独立错误收据；适配器不解释供应商字段或判定目标健康。Core 负责业务检查与恢复，协议失败不能被转换为空的完整错误批次。完整契约见[monitoring](../../monitoring/README.md)。
