# 运行、配置与发布

本文是 runtime path、服务交付、配置示例、项目结构、发布和后续产品演进的唯一事实来源。

## 平台范围

MVP 同时支持 Linux 与 macOS。Broker、协议、CLI/TUI 和展示逻辑保持平台无关，平台差异只进入两个小模块：

- runtime path；
- control peer identity。

Linux 使用 `SO_PEERCRED`，macOS 使用 `LOCAL_PEERPID` 与 `getpeereid`。安全要求见 [安全模型](security.md)。

## Runtime Path

项目使用 `directories::ProjectDirs` 解析平台原生基础目录，具体逻辑集中在 `runtime_path.rs`，不把 Linux/macOS 路径分支散落到 broker、CLI 或安全逻辑。

逻辑用途：

- config：`config.toml`；
- state：Debug Capture 托管目录；
- runtime：control socket 与进程期临时状态；
- cache：不承载配置、审批详情或安全状态。

Linux 结果遵循 XDG 目录约定；macOS 使用 `ProjectDirs` 返回的系统原生用户目录。设计不手写 macOS 绝对路径，当前依赖版本的实际返回值由平台测试固定。

control socket 路径还必须满足目标平台 `sockaddr_un.sun_path` 长度限制。解析出的平台目录无法安全容纳 socket path 时启动失败并给出诊断，不能静默换到共享 `/tmp`、缩短到不可辨识路径或绕过 owner-only 目录要求。

配置采用 TOML，顶层必须包含整数 `schema_version = 1`。文件包含固定 loopback listener、control socket 位置和默认限制，原子写入并使用 `0600`。

反序列化使用 deny-unknown-fields 语义：拼错字段、未知字段、缺少版本、非整数版本或未来版本均为致命配置错误，不能静默忽略或猜测兼容行为。`setup` 只创建 schema v1；`serve` 只读取，不自动迁移或改写。

运行时值按以下优先级解析：

```text
显式 CLI 参数 > config.toml > 内置默认值
```

CLI 覆盖仍必须通过同一类型、范围和安全验证，不能绕过 loopback、路径权限或容量约束。固定 webhook path 不属于配置项。

最小形状：

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

Debug Capture 使用 `ProjectDirs` state 基础目录下的 `debug-captures/`。目录权限为 `0700`，每次启用 capture 的 daemon 启动创建一个新的 `0600` NDJSON 文件，例如：

```text
2026-07-28T02-30-15Z-<daemon-id>.ndjson
```

文件不自动轮换或过期，只由用户显式运行 `nono-approval debug clean` 删除。

## 前台服务

MVP 只直接提供：

```bash
nono-approval serve
```

用户可以在终端、tmux 或自己的进程管理器中启动。CLI 不自动 daemonize。

## systemd 与 launchd

仓库提供：

- systemd user service 示例；
- launchd agent 示例。

示例供用户按实际二进制路径、环境与配置手动调整。CLI 不安装、启用、卸载或修改服务配置，也不要求用户必须采用示例。

## nono Profile 示例

```json
{
  "linux": {
    "af_unix_mediation": "pathname"
  },
  "command_policies": {
    "approval_backends": {
      "local-broker": {
        "type": "webhook",
        "url": "<URL printed by nono-approval setup>",
        "timeout_secs": 300
      }
    },
    "approval_defaults": {
      "backend": "local-broker",
      "timeout_secs": 300
    },
    "commands": {
      "date": {
        "sandbox": {},
        "intercept": [
          {
            "args": [],
            "action": {
              "type": "approve",
              "backend": "local-broker",
              "timeout_secs": 300
            }
          }
        ]
      }
    }
  }
}
```

daemon 默认 Approval Lease 为 `270s`，示例中的 nono timeout 为 `300s`。这 30 秒只给 denied response delivery 留余量；用户修改任一配置后，应继续保持 nono timeout 大于 daemon timeout。

真实使用中，应把安全只读命令放进 `allow`，可能产生副作用的精确参数形状放进 `approve`，绝不允许的操作保留在 `deny`。

同 UID control 隔离不是此示例自动保证的，用户应按 [Profile Validation](security.md#profile-validation) 检查最终 profile。

## 建议代码结构

```text
nono-approval/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── main.rs
│   ├── broker.rs
│   ├── webhook.rs
│   ├── control.rs
│   ├── protocol.rs
│   ├── display.rs
│   ├── interactive.rs
│   ├── runtime_path.rs
│   └── peer_identity.rs
├── tests/
│   ├── webhook.rs
│   ├── control.rs
│   └── lifecycle.rs
├── examples/
│   ├── systemd/
│   └── launchd/
├── README.md
└── LICENSE
```

核心 interface 保持小而稳定：

```rust
pub struct Broker { /* private state */ }

impl Broker {
    pub async fn submit(&self, request: IncomingApproval) -> Result<Decision>;
    pub async fn list(&self) -> Vec<ApprovalSummary>;
    pub async fn show(&self, id: &ApprovalId) -> Result<ApprovalDetail>;
    pub async fn decide(&self, id: &ApprovalId, decision: Decision) -> Result<()>;
}
```

`main.rs` 只解析命令、构造依赖和映射退出码。第一版不引入 database、web UI、plugin system 或 policy engine。

## 依赖选择

- `tokio`：runtime、socket、signal、timeout、oneshot；
- `hyper`、`hyper-util`、`http-body-util`：webhook 与 control HTTP；
- `clap`：CLI；
- `ratatui`、`crossterm`：全屏 TUI；
- `directories`：Linux/macOS 平台原生 config、state/cache 与 runtime 基础目录；
- `serde`、`serde_json`：wire/control DTO；
- `tracing`：结构化运行日志；
- `nix` 或受控 `libc`：peer credentials；
- OS 随机源：approval ID。

生产依赖不包含 nono crate。

## 许可与发布

- MIT License；
- crates.io 发布 Rust CLI；
- GitHub Releases 发布版本与预编译产物。

crates.io 支持源码安装：

```bash
cargo install nono-approval
```

GitHub Releases 提供四个目标：

```text
x86_64-unknown-linux-gnu
aarch64-unknown-linux-gnu
x86_64-apple-darwin
aarch64-apple-darwin
```

每个目标发布一个 `.tar.gz`，包含：

- `nono-approval` 二进制；
- `LICENSE`；
- `README.md`；
- 适用于该平台的 systemd user service 或 launchd agent 示例。

Release 同时提供 `SHA256SUMS`，覆盖所有归档。MVP 不提供 musl target、deb/rpm、Homebrew tap、curl 安装脚本或其他系统包；这些分发渠道需要独立维护和验证，不进入首版发布链。

首个公开版本还必须包含完整 crates.io package metadata 和可复现的 release 构建说明。

## 后续演进

MVP 之后可以单独设计：

- KDE/系统通知、tray、KRunner 或独立窗口；
- TLS、身份、签名与审计完备的远程审批；
- 请求 nono 生成供用户审查的 profile 草稿；
- 由 nono 原生提供 Unix-socket local broker、审批 CLI 或共享协议类型。

session 级或永久批准必须由 nono 自己执行与审计，不能由本地 UI daemon 私自缓存 granted 结果。
