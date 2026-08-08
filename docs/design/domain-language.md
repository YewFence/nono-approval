# 领域语言

本页定义 nono 将一次性人工审批交给本地外部进程时使用的共同语言。专题文档和实现
中的模块命名应使用这些术语，避免同一概念出现多套名字。

## Webhook Endpoint

nono 提交 Approval Request 的本地 HTTP 地址。path 固定，listener host/port 来自
配置或显式 CLI 覆盖；修改后必须同步更新 nono profile。

_避免使用_：Dynamic URL、token path。

## Setup

显式创建或验证本地配置、输出当前 Webhook Endpoint，并打印 nono 集成片段的初始化
操作。

_避免使用_：first-run initialization、automatic setup。

## Profile Validation

用用户指定的 nono profile 启动一次短生命周期 sandbox，并在其中探测 control
socket 实际可达性的非强制检查。结果只描述本次检查，不证明后续 Agent 使用相同配置。

_避免使用_：static profile validation、profile enforcement、launch wrapper、
after-hook validation。

## Wire Adapter

把 nono webhook JSON 转换成本地已知审批请求的兼容 seam，不依赖 nono 的运行时
Rust 类型。

_避免使用_：shared ApprovalRequest type、raw JSON approval。

## Approval Lease

Approval Request 可以被决定的有限有效期，只由 daemon 的单调时钟 deadline
界定。HTTP 连接、本地 wall-clock 倒计时和 control 轮询都不是 Lease。

_避免使用_：connection lifetime、webhook timeout、request age。

## Tombstone

请求完成后，为短期状态解释保留的最小非敏感记录。它允许 `show` 返回 approval ID、
终态和完成时间，但不包含请求详情。

_避免使用_：completed request detail、approval history、audit record。

## Ephemeral Approval Detail

审批仍处于 pending 时，为帮助用户判断而在内存中保存并通过 owner-only control
interface 返回的已知请求字段。请求进入终态后立即销毁。

_避免使用_：redacted approval、approval history、persisted request。

## Debug Capture

显式启用后，在项目托管的 owner-only state directory 中为本次 daemon 启动创建
NDJSON 文件，并追加 `request_received` 和 `request_completed` 明文诊断事件的模式。
它不是默认行为，也不是权威审计日志；文件只由显式 `debug clean` 删除。

_避免使用_：default logging、audit log、implicit persistence。
