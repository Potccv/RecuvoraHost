# 外部监控来源适配

`mod.rs` 中的 `RegistryObservationSource` 将 Host `ExtensionRegistry` 的只读发现与轮询结果转换为 Host `ObservationSource`。Host monitoring 维护 MonitorEngine、规则、时效、覆盖与保存的读取位置；故障状态与一次性保存到磁盘由 Core IncidentStore 负责。
