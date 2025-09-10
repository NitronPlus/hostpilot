# TODO - psm 项目任务清单

此文件同时包含**人类可读**任务清单与**机器可读(JSON)** 部分，方便在多台电脑间同步并让助理（Copilot）在切换工作环境后继续未完成的工作。

注意：助理在继续工作前应更新项目根 `TODO.md` 中相应任务的 `status` 字段，并同步调用内部的 `manage_todo_list` 工具以保持 UI 与文件一致。

## 高优先级任务（来自 `src/transfer.rs` 审查）

- [ ] **抽象 SSH 连接与认证**: 提取 `connect_and_auth(server, timeout)`，统一地址解析、handshake、agent+pubkey 认证逻辑，减少重复。
- [ ] **用 channel 替换 queue 锁**: 将 `Arc<Mutex<VecDeque<usize>>>` 改为 `crossbeam::channel` 或 `std::sync::mpsc`，降低锁竞争并简化代码。
- [ ] **提取单文件上传/下载函数**: `upload_one` / `download_one`，集中错误处理与进度更新。
- [ ] **错误收集与报表**: 不再静默忽略重要错误，收集每个失败文件并在结束时汇总（或支持 `--fail-fast`）。

## 中优先级任务（健壮性与性能）

- [ ] **重试与退避策略**: 对 TCP/SFTP 操作添加有限次重试与指数退避（默认 3 次）。
- [ ] **统一超时与 socket 行为**: 始终使用带超时的连接方法并统一设置 read/write 超时。
- [ ] **线程池或 rayon 改造**: 用线程池管理 worker，避免大量 spawn 开销。
- [ ] **限制进度条数量**: 在非 verbose 模式下只显示总进度或限制同时显示的文件进度条数。

## 低优先级任务（测试与文档）

- [ ] **减少 clone 与内存分配**: 重构任务分配以减少不必要的 `clone()` 与大缓冲区分配。
- [ ] **添加集成测试与 CI 场景**: 为 `ts` 命令添加集成测试（或使用 mock SFTP），并在 README/TRANSFER.md 写明边界行为。
- [ ] **参数化常量**: 将 magic numbers（如 `1024*1024`, worker 上限 `6`, 超时秒数）抽为常量或配置项。

---

## 机器可读部分（供 Copilot/脚本解析）

```json
{
  "copilot_todos": [
    {"id":1,"title":"审查 src/transfer.rs","description":"检查并发、错误处理、资源释放、路径/通配符处理、进度条与日志，列出改进点并实现补丁","status":"completed"},
    {"id":2,"title":"检查 CLI 与分发一致性","description":"确认 `src/cli.rs`、`src/main.rs`、`src/commands.rs` 子命令定义与文档一致，修复未暂存改动并保证编译通过","status":"in-progress"},
    {"id":3,"title":"并发实现改进提案","description":"用线程池或 channel 重构 worker 池，减少 Arc<Mutex> 使用","status":"not-started"},
    {"id":4,"title":"增强错误处理与重试策略","description":"为 SSH/SFTP 添加超时、重连与清晰错误上下文，避免 expect/unwrap","status":"not-started"},
    {"id":5,"title":"性能与内存优化","description":"减少不必要的 clone，优化缓冲区，评估并发上限","status":"not-started"},
    {"id":6,"title":"新增功能：失败输出文件","description":"为 ts 添加 `--output-failures <file>` 选项，把最终失败列表写入指定文件（便于自动化处理）","status":"not-started"},
    {"id":7,"title":"测试与文档补充","description":"更新 `TRANSFER.md` 为 `ts` 的使用示例并添加单元测试（例如 wildcard_match、路径解析）","status":"not-started"},
    {"id":8,"title":"集成/端到端测试","description":"在测试服务器上执行 E2E：上传/下载/目录/glob/并发压力测试，记录步骤与示例命令供 CI 使用","status":"not-started"},
    {"id":9,"title":"日志与诊断改进","description":"增强 tracing 输出（会话 id、文件名、重试次数），并在 --verbose 下输出详细调试信息","status":"not-started"},
    {"id":10,"title":"代码风格与 Lint","description":"运行 cargo fmt & clippy，修复警告（未使用变量、未必要的 mut 等）","status":"not-started"},
    {"id":11,"title":"提交与中文 Commit","description":"把上述改动拆成小 commit，commit message 使用中文，并在 PR 描述列出测试步骤","status":"not-started"},
    {"id":12,"title":"文档：变更记录更新","description":"在 CHANGELOG.md 或 README 中记录 stsf -> ts 的重命名与新增选项及行为改变","status":"not-started"},
    {"id":13,"title":"单元测试补充","description":"为 wildcard_match、路径解析/展开逻辑等纯函数添加单元测试","status":"not-started"},
    {"id":14,"title":"写入仓库 TODO.md","description":"把审查和改进点写入仓库根 TODO.md，并保证机读 JSON 与内部 todo 同步","status":"completed"}
  ]
}
```

## 当前进行中

- `检查 CLI 与分发一致性` (id=2) 已标记为 **进行中**，我会从校验 `src/cli.rs` 与 `src/main.rs` 的参数签名和文档示例开始。

---

如何继续：

- 当你在另一台机器上继续工作，先运行仓库根 `TODO.md` 顶部提示的同步流程（例如通过 git 拉取最新代码），然后让助理或脚本读取并解析 `copilot_todos` JSON，选择要开始的 `id` 并把该任务的 `status` 更新为 `in-progress`，随后调用 `manage_todo_list` 工具保持内部状态一致。

如果你希望，我可以现在：
- （推荐）开始实现第 1 步 `connect_and_auth` 抽象与第 2 步 channel 改造，并在本地运行 `cargo build` 验证；或者按你指定的顺序逐项实现。

---

文件由自动化脚本生成，若需机器解析请读取最后的 JSON 区块 `copilot_todos`。
