# nono-approval

`nono-approval` 是一个本地审批守护进程。它通过 nono 0.69 的同步 webhook ApprovalBackend 接收待审批操作，再把人工交互移到另一个终端中的 TUI 或精确 CLI 命令，避免全屏 Agent TUI 与 nono terminal backend 争用同一个 TTY。

它不会执行命令、代理密码、修改 nono profile 或绕过 nono 的 hard deny；每次决定只对应一个完整 approval ID，并且只生效一次。

## 平台与安装

MVP 支持 Linux 与 macOS：

```bash
cargo install nono-approval
```

也可以从源码构建：

```bash
mise run build
```

## 快速开始

创建 owner-only 配置并打印 nono profile 片段：

```bash
nono-approval setup
```

把输出片段合入最终 nono profile 后，在前台启动 daemon：

```bash
nono-approval serve
```

另开一个终端进入交互审批：

```bash
nono-approval
```

TUI 中 `a` 立即批准、`d` 用固定理由立即拒绝、`D` 输入自定义拒绝理由，`q` 退出。浏览态 Enter 不会批准任何请求。

脚本化控制命令：

```bash
nono-approval status
nono-approval list --json
nono-approval show appr_0123456789abcdef
nono-approval approve appr_0123456789abcdef
nono-approval deny appr_0123456789abcdef --reason "outside this task"
```

`show`、`approve` 和 `deny` 只接受 `appr_` 加 16 位小写十六进制字符的完整 ID，不支持前缀、latest 或 all。

## nono 配置要点

默认 webhook endpoint：

```text
http://127.0.0.1:17443/v1/webhooks/approval
```

daemon 的默认 Approval Lease 是 `270s`；`setup` 输出的 nono backend/default timeout 是 `300s`，为明确 denial 的 HTTP 响应交付留出 30 秒余量。修改任一侧时，应继续保持 nono timeout 大于 daemon timeout。

Unix control socket 的 `0700` 父目录、`0600` 文件权限和 peer UID 校验无法隔离同 UID sandbox。Linux profile 应启用 pathname AF_UNIX mediation，macOS 应使用能限制 Unix socket 的 restricted network mode。启动 daemon 后可以执行真实探针：

```bash
nono-approval config validate --profile <name-or-path>
```

只有 sandbox 已启动且连接明确返回 `EACCES` 或 `EPERM` 才算通过。该命令可能额外运行一次 profile 的宿主侧 session hooks。

## 配置

`setup` 创建平台原生配置目录中的 `config.toml`：

```toml
schema_version = 1

[webhook]
listen = "127.0.0.1:17443"

[approval]
request_timeout = "270s"
max_pending = 64
max_per_session = 8
max_body = "256KiB"
```

未知字段、缺少或不支持的 schema version、非 loopback listener 和不安全文件权限都会导致失败。`serve` 不会隐式创建、迁移或改写配置。运行值按显式 CLI 参数、配置文件、内置默认值的顺序解析。

## Debug Capture

正常模式不把审批详情写入磁盘或普通日志，请求进入终态后立即销毁明文详情。需要诊断时显式启用：

```bash
nono-approval serve --debug-capture
nono-approval debug captures
nono-approval debug clean
```

每次 daemon 启动都会在 owner-only state directory 中创建独立 `0600` NDJSON。文件不会自动轮换或过期；`debug clean` 只删除经过 owner、类型和固定命名验证的托管文件，不递归删除目录。

## Shell completion

```bash
nono-approval completions bash
nono-approval completions zsh
nono-approval completions fish
```

## 开发与验证

```bash
mise run test
mise run check
```

架构、安全边界、协议和验证现状见 [`docs/design/overview.md`](docs/design/overview.md)。

## License

MIT
