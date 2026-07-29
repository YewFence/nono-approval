# Getting Started

## Installation

```bash
cargo install nono-approval
nono-approval setup
```

将 `setup` 输出的 approval backend 片段合入最终 nono profile。

## Usage

在一个终端启动 daemon：

```bash
nono-approval serve
```

在另一个终端运行 `nono-approval` 打开 TUI。按 `a` 批准、`d` 快速拒绝、`D` 填写理由后拒绝。浏览态 Enter 不执行审批决定。

## Configuration

配置由 `setup` 创建，必须包含 `schema_version = 1`。默认 webhook 为 `127.0.0.1:17443`，Approval Lease 为 `270s`，pending 上限为全局 64、每 session 8，请求体上限为 `256KiB`。

运行 `nono-approval config validate --profile <name-or-path>` 可以通过真实 nono sandbox 检查 control socket 隔离；只有 `EACCES` 或 `EPERM` 才判定通过。
