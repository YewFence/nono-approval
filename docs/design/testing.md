# 实现与验证计划

本文是实现阶段、自动化测试和真实 nono 验证的唯一事实来源。

## 实现顺序

### Phase 1：Webhook Bridge

1. 初始化独立 Cargo package，保留 `lib.rs`；
2. 定义独立 wire DTO 与 webhook response；
3. 实现 Broker、内存 pending store 与 oneshot；
4. 实现固定 loopback webhook server；
5. 通过进程内 interface 完成一次性 approve/deny；
6. 验证 nono 能阻塞并在决定后继续或拒绝。

完成标准：command approval 可登记，approve 成功继续，deny 与 timeout fail closed。

### Phase 2：Control 与 TUI

1. 创建 owner-only runtime directory 与 Unix socket；
2. 实现 status/list/show/decision API；
3. 实现对应 CLI；
4. 增加 Linux/macOS peer UID 验证；
5. 增加终端安全展示；
6. 实现轮询式 `ratatui + crossterm` 审批界面。

### Phase 3：安全与健壮性

1. body、global pending、per-session 上限；
2. duplicate/replay cache；
3. best-effort disconnect cleanup；
4. shutdown denial；
5. 明文详情生命周期；
6. Profile Validation 真实探针；
7. Debug Capture；
8. JSON 运行日志。

### Phase 4：交付

1. systemd user service 与 launchd agent 示例；
2. shell completion；
3. profile 配置指南；
4. Pi、Claude Code、Codex 端到端验证；
5. MIT License、crates.io metadata 与四目标 GitHub Release 构建；
6. 为全部 `.tar.gz` 生成并验证 `SHA256SUMS`。

## 单元测试

### Broker 与生命周期

- 创建请求和唯一 approval ID；
- approval ID 为 8 个随机字节编码的 16 位小写十六进制，碰撞时重生成；
- approve、deny、timeout、cancel；
- 重复 request ID；
- 两个并发决定只有一个成功；
- global `64` / per-session `8` capacity 边界；
- per-session 满返回 `429`，全局满返回 `503`；
- 容量拒绝不生成 approval ID、Tombstone 或 Debug Capture 记录；
- replay cache；
- terminal state 后销毁 request detail；
- Tombstone 不包含请求详情并遵守 1024/10 分钟上限；
- 轮询不延长 Approval Lease；
- 默认 Approval Lease 为 `270s`，并独立于 nono 示例中的 `300s` timeout；
- shutdown 结束全部 pending。

### Protocol

- command、endpoint、capability、network fixture；
- 已知 variant 的未知附加字段；
- unknown variant；
- 缺少公共字段；
- trailing JSON；
- body 恰好位于 `256 KiB` 边界；
- oversized body 在解析前返回 `413`，不进入 pending、日志或 Debug Capture；
- invalid UTF-8/JSON；
- granted/denied response shape。

### CLI ID

- 带 `appr_` 的完整 ID 精确匹配；
- 缺少前缀拒绝；
- 非 16 位 hex、含大写或其他编码形式拒绝；
- 唯一但不完整的前缀仍拒绝；
- 未知和已结束 ID 不回退到其他请求。

### Config

- `setup` 原子创建权限 `0600` 的 `config.toml` 与 `schema_version = 1`；
- `setup` 对有效既有配置幂等；
- 缺少版本、未来版本、未知字段、字段拼错和非法 TOML 都返回非零；
- 无效既有配置不会被 `setup` 覆盖或被 `serve` 自动修复；
- `serve` 只读取配置，不迁移或改写文件；
- 非秘密参数遵循 `CLI > config.toml > 内置默认值`；
- CLI 覆盖仍执行与配置值相同的范围和安全验证；
- 固定 webhook path 不进入配置，也没有覆盖入口。

### Display

- ANSI、OSC hyperlink、terminal title sequence；
- embedded newline、C0/C1 control characters；
- `list` summary 按列宽使用明确省略标记截断；
- `show` 与 TUI 长参数自动换行且不截断；
- 安全转义后详情恰好 `1 MiB` 可进入 pending，超限返回 `422` 且不进入日志或 Debug Capture；
- token/password/query 等值仍保留可判断的明文内容；
- quoting 不可执行化。

### Interactive UI

- 无需复制 ID 即可选择并决定；
- 空队列等待与新请求出现；
- daemon 尚未启动时显示等待并每 `1s` 重连，`q` 可退出；
- daemon 出现后无需重启 TUI 即切换到 `500ms` 正常轮询；
- 等待 TUI 不隐式执行 setup 或启动 daemon；
- 已连接 daemon 断开时立即清除旧 snapshot、选择、详情、滚动和 reason 草稿；
- 运行中断线后显示 disconnected 并每 `1s` 重连，新 daemon 状态从空客户端模型重新加载；
- 旧请求在断线期间不可操作，即使新 daemon 返回相同 ID 也不复用旧状态；
- `500ms` 轮询与本地倒计时；
- queue 按 `received_at` FIFO 稳定排序，相同时间以 approval ID 打破平局；
- 新请求追加且不抢占当前选择或重置滚动；
- 当前项消失后优先选原位置下一项，没有时回退上一项；
- 双栏和单栏布局稳定；
- 双栏和单栏详情按内容宽度自动换行，不提供横向滚动；
- resize 后重新换行并尽可能保持逻辑阅读位置；
- resize 按 approval ID 保留选择；
- 键位语义固定，普通浏览态 Enter 不决定，任何状态下 Enter 都不能批准；
- `d` 以固定 reason 立即拒绝，`D` 进入理由输入态；
- 理由输入态 Enter 提交 denial，Esc 取消并丢弃输入；
- 空理由不提交，UTF-8 编码后恰好 `4 KiB` 可提交，超限明确失败且不截断；
- TUI、CLI `--reason` 与 control API 使用相同 reason 验证；
- 编辑理由期间目标 ID 固定，请求结束后提交不得作用到其他项目；
- 详情滚动不改变选择；
- 并发新增、完成、过期不会把决定落到错误请求；
- 超长内容与小终端不重叠；
- 正常、错误、panic 与 signal 路径恢复终端状态。

### Debug Capture

- 托管 state directory 的 owner、权限和 symlink 验证；
- 每次启用的 daemon 启动创建独立 `0600` 捕获文件；
- `schema_version` NDJSON 逐行追加；
- `request_received` 与 `request_completed` 字段；
- completion 不重复完整 Wire DTO；
- 不记录 control 轮询和 UI 行为；
- 普通日志不混入明文详情；
- 读取方可忽略末尾不完整行；
- `debug captures` 只列 metadata，不打印捕获内容；
- `debug clean` 删除全部合法托管文件且不二次确认；
- `debug clean` 拒绝 symlink、子目录、异常 owner 和非托管文件名，不递归删除目录；
- 启动时无法安全创建捕获文件则 daemon 启动失败；
- 运行中首次追加失败会关闭捕获、只记录一次错误并继续审批；
- 捕获失败后 `status` 和 TUI 持续显示 failed，且不重试或切换文件。

## 集成测试

启动真实 daemon：

1. webhook handler 在没有决定时保持连接；
2. approve API 返回 granted；
3. deny API 返回 reason；
4. daemon deadline 自动拒绝；
5. 可观察断开会 best-effort 清理；
6. 即使连接未断开，deadline 后也不能批准；
7. shutdown 结束全部 pending；
8. control socket 权限正确；
9. 不同 UID control client 被拒绝；
10. 非 loopback bind 默认拒绝；
11. 固定 endpoint 接受合法本地请求，其他 path 返回 `404`；
12. stale socket 只在安全条件下清理；
13. Debug Capture 创建失败阻止启动，运行时写失败只关闭捕获而不影响审批。

平台 CI 至少覆盖 Linux 与 macOS 的编译、runtime path 权限检查、peer identity socket-pair 测试和 TUI 渲染核心状态测试。平台 API 失败时 control connection 必须 fail closed。

平台目录测试使用固定版本的 `directories::ProjectDirs`，验证 Linux/macOS 的 config、state/cache 与 runtime 用途映射、owner-only 项目目录创建，以及 control socket pathname 超过平台限制时明确启动失败且不回退到共享临时目录。

## Profile Validation 测试

- daemon 未运行返回非零；
- nono/profile/sandbox 初始化失败返回非零；
- 探针未报告 started 返回非零；
- `EACCES/EPERM` 才通过；
- ENOENT、ECONNREFUSED、timeout 等都不误报安全；
- control HTTP 可达判为不安全；
- 探针不创建或决定 Approval Request；
- 文档和 stderr 明确提示 session hook 副作用。

## nono 端到端测试

准备临时 profile，把 command policy 指向测试 daemon。验证：

1. 普通命令行下 nono 发出 webhook；
2. TUI/CLI 能看到完整操作；
3. approve 后原命令成功；
4. deny 后原命令明确失败；
5. daemon 默认 `270s` Lease 比 nono 默认 `300s` timeout 更早结束；
6. Pi TUI 中审批在另一个终端完成，不争用 Pi 的 tty；
7. endpoint/capability fixture 与当前 nono schema 兼容。

## MVP 验收

- 使用当前 nono webhook backend，无需修改 nono；
- request 可登记为 pending 且只能决定一次；
- 不带子命令运行时进入交互 TUI；
- 空队列等待，新请求自动显示；
- TUI 与精确 CLI 都不二次确认；
- Enter 永不批准；
- list/show 仍支持脚本化查询；
- deadline、shutdown 和错误路径 fail closed；
- body 默认上限为 `256 KiB`，pending 默认全局 `64`、每 session `8`；
- control socket owner-only 且验证 peer UID；
- Profile Validation 只有真实 `EACCES/EPERM` 才通过；
- webhook 只监听 loopback；
- 明文详情经过终端安全转义；
- 正常模式不落盘、不写日志，terminal state 后销毁详情；
- Debug Capture 只显式写入托管 owner-only NDJSON，不自动轮换或过期；
- `debug clean` 可安全删除全部合法托管捕获文件；
- 提供 systemd/launchd 示例但不自动配置；
- MIT、crates.io 与 GitHub Releases 交付材料完备；
- GitHub Release 覆盖 Linux/macOS 的 x86_64/aarch64 GNU/Darwin 四目标；
- 每个归档包含二进制、LICENSE、README 和平台适用的服务示例，`SHA256SUMS` 校验通过；
- MVP 发布流水线不包含 musl、deb/rpm、Homebrew tap 或安装脚本；
- Linux/macOS 自动化和真实 nono E2E 通过。
