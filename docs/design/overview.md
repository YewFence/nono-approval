# 架构总览

`nono-approval` 是一个本地审批守护进程。它通过 nono 的同步 webhook
ApprovalBackend 接收待审批操作，再把人工交互移到另一个终端中的 TUI 或精确
CLI 命令，避免全屏 Agent TUI 与 nono terminal backend 争用同一个 TTY。

本文只描述产品边界、模块及其接口。协议字段、状态转换、安全约束、交互行为、
平台路径和验证证据由对应专题文档维护。

## 产品边界

`nono-approval` 只返回用户对一个精确 Approval Request 的一次性决定：

```text
Approve exactly this request once
Deny exactly this request once
```

daemon 不执行请求中的操作，不代理 sudo 或密码，不修改 nono profile，也不覆盖
nono 的 hard deny、protected roots 或平台 sandbox 约束。

当前实现支持：

- nono `0.69` 同步 webhook ApprovalBackend；
- command、endpoint、capability 和 network 四种 wire variant；
- owner-only Unix control socket 上的 HTTP control interface；
- 精确 ID 的 `status/list/show/approve/deny` CLI；
- 不带子命令时启动的轮询式全屏 TUI；
- Linux 与 macOS 的平台路径和 peer identity adapter；
- 显式、owner-only 的 NDJSON Debug Capture。

不支持：

- session 级或永久批准；
- 自动判断操作是否安全、自动批准或生成策略；
- daemon 重启后恢复 pending request；
- 跨主机审批、浏览器 UI 或通用策略引擎；
- 将普通日志或 Tombstone 作为权威审计日志；
- 在同 UID 进程已经可以访问 control socket 时额外区分宿主用户与 sandboxed Agent。

## 总体结构

```text
┌────────────────── nono ──────────────────┐
│ Tool Sandbox / proxy / supervisor        │
│ ApprovalBackend::request_approval        │
└───────────────────┬──────────────────────┘
                    │ loopback HTTP webhook
                    ▼
┌────────────── nono-approval serve ──────────────┐
│ Wire Adapter -> Broker/PendingStore             │
│        ├── Approval Lease / Tombstone           │
│        ├── Debug Capture（显式启用）             │
│        └── HTTP over owner-only Unix socket     │
└───────────────────┬─────────────────────────────┘
                    ▼
            CLI / interactive TUI
```

生产依赖图不包含 nono crate。Wire Adapter 在本项目内定义当前兼容 DTO；真实 nono
是外部 wire 事实来源，而不是运行时库依赖。

## 模块与接口

### Wire Adapter

Wire Adapter 是 webhook JSON 与本地已知请求之间的兼容 seam。它负责尺寸限制、
已知 variant 解析、必填字段验证和终端安全展示模型构造。未知 variant 或无法完整
展示的请求 fail closed。

详见[协议与适配](protocol.md)。

### Broker

Broker 是审批生命周期的核心模块。它登记请求、分配 approval ID、执行容量和重放
限制、维护 daemon 单调时钟 Approval Lease，并把每个请求的一次性决定送回对应的
webhook handler。

调用方只通过 submit、list、show、decide 和 shutdown 行为接触它；pending 详情、
oneshot、Tombstone 和 replay cache 都封装在模块内部。

详见[审批生命周期](approval-lifecycle.md)。

### Webhook 与 Control adapter

Webhook adapter 只在 loopback TCP 上接收 nono 请求。Control adapter 在 Unix
socket 上向 CLI、TUI 和 Profile Validation probe 提供 HTTP interface。两个入口
共享 Broker，但没有共享监听地址或认证模型。

详见[协议与适配](protocol.md)和[安全模型](security.md)。

### CLI 与 TUI

CLI 和 TUI 都是 control interface 的客户端，不直接访问 Broker 内部状态。决定
始终携带完整 approval ID，因此队列刷新、并发客户端或请求消失都不能把一次操作
转移到另一条请求。

详见[CLI 与 TUI](cli-and-tui.md)。

### 平台 adapter

平台差异集中在原生项目目录解析和 control peer identity：Linux 使用
`SO_PEERCRED`，macOS 使用 `LOCAL_PEERPID` 与 `getpeereid`。其余 Broker、协议、
展示和交互逻辑保持平台无关。

详见[运行、配置与发布](operations.md)。

## 文档地图

- [领域语言](domain-language.md)：Approval Lease、Tombstone、Wire Adapter 等术语；
- [审批生命周期](approval-lifecycle.md)：Broker、deadline、Tombstone、重放与关闭；
- [协议与适配](protocol.md)：webhook schema、HTTP 状态码和 control interface；
- [安全模型](security.md)：信任模型、Unix socket、Profile Validation、日志和 Debug Capture；
- [CLI 与 TUI](cli-and-tui.md)：命令、输出、键位、轮询和布局；
- [运行、配置与发布](operations.md)：平台路径、配置 schema、服务示例和发布产物；
- [验证现状](testing.md)：当前自动化覆盖、CI 和仍需手动验证的行为；
- [nono 0.69 调研](../research/nono-0.69.md)：当前设计依赖的版本化外部事实；
- [ADR](../adr/0001-daemon-deadline-defines-approval-lease.md)：已经收敛的跨模块取舍。

## 实现状态

上述模块均已有实现。专题文档描述当前代码行为；尚未被自动化验证的事项会明确写在
[验证现状](testing.md)中，而不会作为已经完成的保证出现。
