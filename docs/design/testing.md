# 验证现状

本文记录当前仓库已经自动化验证的行为、CI 平台和仍需手动验证的风险。它不是未来实现
计划；功能语义以对应专题文档和代码为准。

## 本地入口

项目通过 mise 暴露可复用检查：

```bash
mise run test        # cargo test --workspace --all-targets --all-features --locked
mise run rust:check  # fmt、cargo check、clippy -D warnings
mise run repo:check  # lockfile、newline、typos、secret 和 workflow 检查
mise run docs:build  # VitePress 构建
mise run check       # repo:check、rust:check、test
```

## 当前单元测试

### Broker 与生命周期

`src/broker.rs` 当前覆盖：

- 完整 approval ID 形状，拒绝缺少前缀、短 ID 和大写 hex；
- denial reason 的空值、全 NUL、`4 KiB` 边界和超限；
- approve 只生效一次，完成后 `show` 返回最小 Tombstone；
- 单调 deadline 到期返回 denial；
- duplicate、per-session 和 global capacity。

### Wire Adapter 与 webhook

`src/protocol.rs` 和 `src/webhook.rs` 当前覆盖：

- command 的附加未知字段兼容；
- 未知或 incomplete variant；
- trailing JSON 和 body limit；
- endpoint、capability、network fixture；
- 非法 access/protocol 和空操作字段；
- 固定 webhook path。

四种 fixture 固定的是本项目当前 DTO 行为，测试依赖图没有引入 nono crate。

### 配置与 runtime path

`src/config.rs` 当前覆盖：

- `setup` 创建 `0600` 文件且幂等；
- 未知字段和不安全文件权限失败。

`src/runtime_path.rs` 当前覆盖：

- owner-only 目录创建和 `0700` 权限；
- symlink path component 拒绝；
- 既有 permissive 目录在只读验证时拒绝。

### 展示与 TUI

`src/display.rs` 当前覆盖 ANSI/OSC 清理、C0 可见转义和 Unicode-width summary 截断。

`src/interactive.rs` 当前覆盖：

- disconnect 清空客户端 request state；
- 宽屏/窄屏基本渲染；
- denial reason 输入态不会把 Enter 解释成 approve。

### Debug Capture

`src/debug_capture.rs` 当前覆盖：

- 创建、列出和清理托管 capture；
- 非托管目录条目拒绝；
- received/completed NDJSON 及 completion reason 安全清理；
- `response_delivery_outcome: not_observed` 序列化。

### CLI

`src/cli.rs` 当前覆盖 partial approval ID 在 clap 解析阶段失败，以及所有公开子命令都有
help 描述。

## 当前集成测试

`tests/bridge.rs` 启动真实 TCP webhook listener 和 Unix control listener，覆盖：

1. webhook request 登记为 pending；
2. control list 找到精确 approval ID；
3. control approve 返回 granted；
4. 原 webhook response 收到 granted；
5. terminal state 后第二次决定返回 conflict；
6. 超过 `8 KiB` 的 control decision body 返回 `400` 且不决定请求。

该集成测试使用临时 socket path 和进程内 Broker，不启动完整 CLI 进程，也不运行 nono。

## CI

GitHub Actions 当前执行：

- Linux 与 macOS 的 `mise run rust:check`；
- Linux 与 macOS 的 `mise run test`；
- Linux repository checks；
- VitePress 文档构建；
- 定期 Rust dependency audit；
- release 时四目标构建、归档、crates.io publish 和 GitHub Release。

可移植检查与打包行为由 `mise.toml`、`mise.ci.toml` 和 `scripts/package-release` 维护；
workflow 负责事件、权限、平台 matrix、artifact 和发布编排。

## 尚未自动化验证

以下行为在代码或文档中存在，但当前测试集没有提供端到端证据：

- 真实 nono `0.69` 从 command policy 发出 webhook，并在 approve/deny 后继续或拒绝操作；
- Pi、Claude Code、Codex 全屏 TUI 中跨终端审批不争用原 TTY；
- `config validate` 对真实 Linux Landlock 和 macOS Seatbelt profile 的可达/拒绝矩阵；
- Profile Validation 的 timeout、invalid child protocol、session hook 提示和各 errno 分支；
- Linux/macOS peer identity 的不同 UID socket-pair 行为和平台 API 失败路径；
- 完整 daemon 收到 `SIGINT`/`SIGTERM` 后的 denial、100ms flush 和 socket 清理；
- webhook 的 method、content-type、所有错误状态码和 disconnect cancellation 的网络级测试；
- Tombstone 1024 条/10 分钟淘汰、replay TTL 和并发双决定的竞争测试；
- Debug Capture 运行时 I/O 失败转为 failed 且审批继续；
- 每个 ProjectDirs 平台实际路径和 socket ABI 长度边界；
- TUI 的全部键位、500ms/1s 节奏、selection fallback、resize、panic/异常终端恢复；
- 四个 release archive 的内容在 CI 中解包验证。

这些条目不能在发布说明或用户文档中表述成已经通过的验收项。补充测试时应直接验证
模块公开 interface，避免为测试另建一套旁路实现。

## 手动验证建议

涉及真实 nono 或平台 sandbox 时，至少记录：

1. nono 精确版本和 profile；
2. 操作系统与架构；
3. daemon 配置和实际 webhook/control 地址；
4. approval ID、终态和原操作结果；
5. 是否启用 Debug Capture；
6. Profile Validation 是否确认 started，及最终 errno/可达结果。

调研事实与手动实验应写入版本化 research 文档，不要混入长期协议承诺。
