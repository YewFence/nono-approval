# nono Local Approval

本上下文描述 nono 将一次性人工审批交给本地外部进程完成时使用的领域语言。

## Language

**Webhook Endpoint**:
nono 提交 Approval Request 的稳定本地地址，在审批守护进程重启后仍保持有效。
_Avoid_: Dynamic URL, daemon address, token path

**Setup**:
显式创建并检查本地审批配置、输出固定 Webhook Endpoint，并给出 nono 集成片段的一次性初始化操作。
_Avoid_: First-run initialization, automatic setup

**Profile Validation**:
用用户指定的 nono profile 显式启动一次短生命周期沙箱，并在其中探测 control socket 实际可达性的非强制检查；结果只描述本次检查，不证明后续 Agent 使用了同一配置。
_Avoid_: Static profile validation, profile enforcement, setup-generated profile, launch wrapper, after-hook validation

**Wire Adapter**:
把 nono webhook JSON 转换成本地已知审批请求的兼容边界，不依赖 nono 的运行时 Rust 类型。
_Avoid_: Shared ApprovalRequest type, raw JSON approval

**Approval Lease**:
Approval Request 可以被决定的有限有效期，只由审批守护进程的单调时钟 deadline 界定。
_Avoid_: Connection lifetime, webhook timeout, request age

**Tombstone**:
请求完成后为 replay detection 和短期状态解释保留的最小非敏感记录，不包含请求详情。
_Avoid_: Completed request, approval history, audit record

**Ephemeral Approval Detail**:
审批仍处于 pending 时，为帮助用户判断而在内存中保存并通过 owner-only control 面明文展示的完整已知请求字段；请求进入终态后立即销毁。
_Avoid_: Redacted approval, approval history, persisted request

**Debug Capture**:
显式启用后，在项目托管的 owner-only state directory 中为本次 daemon 启动创建 NDJSON 文件，并追加 `request_received` 和 `request_completed` 明文诊断事件的模式。它不是默认行为，也不是权威审计日志；文件只由显式 `debug clean` 删除。
_Avoid_: Default logging, audit log, implicit persistence
