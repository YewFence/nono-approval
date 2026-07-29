# Getting Started

## Installation

```bash
cargo install nono-approval
nono-approval setup
```

`setup` 使用平台原生项目目录创建或验证 owner-only `config.toml`，然后输出当前
Webhook Endpoint 和 nono approval backend 片段。将片段合入最终 nono profile；它不会
修改 profile。

## Usage

在一个终端启动 daemon：

```bash
nono-approval serve
```

在另一个终端运行 `nono-approval` 打开 TUI。daemon 尚未启动时 TUI 会等待并每秒重连。
按 `a` 批准、`d` 用固定理由快速拒绝、`D` 填写理由后拒绝；浏览态 Enter 不执行审批决定。

## Configuration

配置由 `setup` 创建，必须包含 `schema_version = 1`。默认 webhook 为
`127.0.0.1:17443`，Approval Lease 为 `270s`，pending 上限为全局 `64`、每 session
`8`，请求体上限为 `256KiB`。control socket 路径由平台项目目录解析，不写入该配置
文件；需要临时指定路径时使用隐藏的 `--control-socket` 参数。

运行 `nono-approval config validate --profile <name-or-path>` 可以通过真实 nono
sandbox 检查 control socket 隔离；只有 sandbox 已报告启动且连接返回 `EACCES` 或
`EPERM` 才判定通过。命令可能额外执行目标 profile 的宿主侧 session hooks。

## 下一步

- [架构总览](../design/overview.md)
- [CLI 与 TUI](../design/cli-and-tui.md)
- [运行、配置与发布](../design/operations.md)
- [验证现状](../design/testing.md)
