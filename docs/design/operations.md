# 运行、配置与发布

本文描述当前实现的项目路径、配置加载、服务运行方式、平台 adapter、仓库结构和
发布产物。安全细节见[安全模型](security.md)。

## 平台范围

当前代码在 Linux 与 macOS 上实现 control peer identity：

- Linux：`nix` 的 `SO_PEERCRED`；
- macOS：`nix` 的 `LOCAL_PEERPID` 与 `getpeereid`；
- 其他平台：peer identity adapter 返回 unsupported，因此不能安全提供 control service。

生产 crate 全局 `unsafe_code = "forbid"`，不直接调用 `libc`。Broker、协议、展示和
TUI 不包含平台分支。

## 项目路径

`ProjectPaths::resolve()` 使用：

```rust
ProjectDirs::from("dev", "YewFence", "nono-approval")
```

并解析以下路径：

| 用途 | 解析方式 |
| --- | --- |
| config | `ProjectDirs.config_dir()/config.toml` |
| state | `ProjectDirs.state_dir()`；没有 state dir 时回退到 `data_local_dir()` |
| runtime | `ProjectDirs.runtime_dir()`；没有 runtime dir 时回退到 `data_local_dir()/runtime` |
| control | `runtime/control.sock` |

这意味着 Linux 通常遵循 XDG，macOS 使用 `ProjectDirs` 提供的原生用户目录。文档不把
`$XDG_CONFIG_HOME` 或 `$XDG_RUNTIME_DIR` 当成跨平台 interface，也不硬编码 macOS
绝对路径。

control socket path 必须符合平台 `sockaddr_un.sun_path` 长度限制：Linux 当前按
`107` 字节检查，macOS 按 `103` 字节检查。超长路径启动失败，不回退到共享
`/tmp`。

## 配置文件

配置是严格 TOML schema：

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

`ConfigFile` 当前只有 `schema_version`、`webhook` 和 `approval` 三组字段；control
socket 不写入 config，由平台默认路径或 CLI `--control-socket` 决定。未知字段、缺少
版本、非整数版本、非法 TOML、非 loopback listener、零限制和
`max_per_session > max_pending` 都会失败。

文件必须是当前用户拥有的 regular file，不能是 symlink，权限必须精确为 `0600`。
`setup` 首次创建时原子写入；`load` 和 `serve` 只读取，不迁移或修复文件。

运行时值按以下顺序覆盖：

```text
显式 CLI 参数 > config.toml > 内置默认值
```

CLI override 继续执行同样的 loopback、正数和容量关系验证。

## 前台 daemon

当前直接运行前台 daemon：

```bash
nono-approval serve
```

CLI 不自动 daemonize。可以由用户放入 tmux、自己的进程管理器或仓库提供的服务示例。
收到 `SIGINT`/`SIGTERM` 后，Broker 将 pending request 变为 denial，等待 `100ms`，
随后停止 server task 并删除 socket。

## 服务示例

仓库当前提供：

- `examples/systemd/nono-approval.service`；
- `examples/launchd/dev.yewfence.nono-approval.plist`。

它们是供用户调整路径、环境和启动参数的示例，不由 CLI 安装、启用、卸载或修改。

## nono profile 集成

`setup` 输出的最小片段只包含 webhook backend 和 approval defaults：

```json
{
  "command_policies": {
    "approval_backends": {
      "local-broker": {
        "type": "webhook",
        "url": "http://127.0.0.1:17443/v1/webhooks/approval",
        "timeout_secs": 300
      }
    },
    "approval_defaults": {
      "backend": "local-broker",
      "timeout_secs": 300
    }
  }
}
```

如果需要隔离同 UID sandbox 对 control socket 的访问，用户还要在最终 profile 中配置
平台对应的 Unix socket/network mediation；`setup` 不生成或强制 profile。真实行为用
`nono-approval config validate --profile ...` 验证，详见[安全模型](security.md#profile-validation)。

## 代码结构

当前仓库的主要实现文件为：

```text
src/
├── main.rs                 # 进程入口
├── lib.rs                  # crate 导出与版本
├── cli.rs                  # clap 命令与退出路径
├── daemon.rs               # listener、task 和 shutdown
├── webhook.rs              # loopback HTTP ingress
├── control.rs              # Unix-socket HTTP control
├── broker.rs               # pending、decision、Lease、Tombstone
├── protocol.rs             # Wire Adapter DTO 与解析
├── display.rs              # 安全清理、summary 和 detail
├── interactive.rs          # ratatui TUI
├── config.rs               # TOML schema 和原子 setup
├── runtime_path.rs         # ProjectDirs 与 owner-only 路径
├── peer_identity.rs        # Linux/macOS peer UID
├── profile_validation.rs   # nono sandbox probe
└── debug_capture.rs        # NDJSON capture
tests/bridge.rs             # webhook/control bridge 集成测试
```

模块间 interface 以 `Broker`、`ControlClient`、`KnownApprovalRequest`、`ProjectPaths`
和 `DebugCapture` 为主要 seam；没有 database、web UI、plugin system 或 policy
engine。

## 依赖与工具

核心依赖按职责分组：

- Tokio：async runtime、socket、signal、timeout、oneshot；
- Hyper/hyper-util/http-body-util：webhook 和 control HTTP；
- Clap/clap_complete：CLI 和 completion；
- Ratatui/crossterm：TUI；
- directories：平台项目目录；
- serde/serde_json/toml：wire、control 和 config DTO；
- nix：Linux/macOS peer credential 安全封装；
- vte、shlex、unicode-width、textwrap：终端清理、展示 quoting 和布局；
- blake3、getrandom、tempfile、jiff：进程期哈希、随机数、原子文件和时间处理。

生产依赖不包含 nono crate。

## 发布现状

仓库已有 crates.io 和 GitHub Releases 发布任务。GitHub Release 构建四个目标：

```text
x86_64-unknown-linux-gnu
aarch64-unknown-linux-gnu
x86_64-apple-darwin
aarch64-apple-darwin
```

每个归档由 `scripts/package-release` 生成 `.tar.gz`，包含：

- `nono-approval` 二进制；
- `README.md`；
- `LICENSE`；
- Linux 的 systemd service 示例，或 macOS 的 launchd agent 示例。

当前发布链没有 musl、deb/rpm、Homebrew tap 或 curl 安装脚本。

## 本地开发入口

项目使用 mise 组织可复用任务：

```bash
mise run check       # 仓库检查、格式、编译、lint、测试
mise run test        # Rust 测试
mise run docs:build  # VitePress 文档构建
mise run build       # release 构建
```

CI workflow 只负责平台和 GitHub Actions 编排，实际可移植检查由 `mise.toml` 与
`mise.ci.toml` 任务提供。
