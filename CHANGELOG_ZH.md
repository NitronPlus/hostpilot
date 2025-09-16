## v0.9.0

本次发布整合了若干已实现的功能与可靠性改进，最显著的是内置 SFTP 传输命令（`hp ts`）的实现与对遗留配置数据的迁移路径。

要点
- 内置 SFTP 传输（`hp ts`）现为脚本化传输的主力实现，采用工作池（worker-pool）并行模型，支持上传与下载两种方向。
- 自动配置与数据迁移：实现了对遗留 `~/.psm` 的迁移、对旧 `server.json` 的备份与迁移到 SQLite `server.db` 的流程（见下文升级路径）。

实现细节（基于源码）
- `hp ts`（传输）:
  - 方向检测：通过 `alias:/path` 语法判断上传或下载，支持混合源/目标输入。
  - 并发：可配置的 worker 池；CLI 选项 `-c/--concurrency` 控制 worker 数（CLI 默认取 6，程序中限制最大为 8）。每个 worker 会话独立建立 SSH 连接并并行传输文件。
  - 上传：支持本地文件、目录及尾部斜杠语义，使用 `walkdir` 递归采集文件，按需在远端创建父目录。
  - 下载：支持远端单文件、目录以及简单通配模式；通过 SFTP 枚举远端目录并写入本地文件，按需创建本地目录。
  - 进度展示：使用 `indicatif::MultiProgress` 显示汇总与文件级进度条，完成时打印平均速率摘要。
  - 认证：优先尝试 SSH agent，其次尝试常见密钥文件（`~/.ssh/id_ed25519`, `id_rsa`, `id_ecdsa`）通过 `ssh2` 完成认证。
  - 可靠性：传输工作线程将错误收集到共享列表并在结束时打印失败汇总；使用连接超时与读写超时避免阻塞。
  - 路径处理：支持在远端展开 `~`（通过执行 `echo $HOME`），并对本地 `.` 目标进行规范化，构建远端路径时对 Windows 路径做兼容处理。

- 配置与迁移：
  - 运行时会检测并迁移遗留 `~/.psm`（优先重命名，重命名失败时回退为递归复制）。
  - 如果检测到旧的 `server.json` 或 `server.db`，升级逻辑会备份现有文件、将服务器记录迁移到 SQLite 数据库（`server.db`，包含 id、alias 唯一、username、address、port、last_connect 字段），并更新 `config.json` 指向新的数据库与协议版本。
  - `ops.rs` 中提供备份、创建 SQLite schema 以及自动升级流程的实现。

- CLI 与命令（代码中确认）：
  - `hp new <alias> user@host[:port]` —— 创建别名（通过 `parse::parse_remote_host` 做解析与校验）。
  - `hp ls` —— 列出别名（表格显示）。
  - `hp rm <alias>` —— 删除别名。
  - `hp mv <alias> <new_alias>` —— 重命名别名。
  - `hp ln <alias>` —— 将本地公钥安装到远端（优先使用 `ssh-copy-id`，回退为通过 ssh stdin 追加到 `authorized_keys` 的安全脚本）。
  - `hp ts <sources...> <target> [-c N] [--verbose]` —— 内置 SFTP 传输（参见 `transfer.rs`）。
  - `hp set` —— 更新配置（公钥路径、server 文件路径、ssh 客户端路径、scp 路径）；`-k` 设置公钥路径，`-a` 设置 scp 路径（详见 `cli.rs`）。

注意与已知限制
- `hp ts` 当前使用 `ssh2`（libssh2），不支持交互式密码提示 —— 无密码/脚本化场景需使用公钥认证或 agent。
- 代码快照中未实现显式断点续传与校验（如 `--checksum`）或原子 `.part` 恢复策略；传输会尽力完成并在失败时记录错误，但没有完整的续传控制接口。
- 高级功能（例如 `hp import`/`export`、`hp info`、dry-run 或 checksum 模式）在当前源码中未找到，因此未列为已实现特性。

维护
- 代码包含用于 TUI（终端 UI）搭建的辅助函数，并在若干点使用 `tracing` 提供调试位置以便排查问题。

---

## v0.5.0

* 别名创建与更新的可用性改进：
  - `hp new` 及相关参数解析增强以接受 `user@host[:port]` 格式，并对格式错误提供更清晰的校验与错误提示。
  - 帮助文本与用例更新为更简洁的用法示例（例如 `hp new example user@host[:port]`）。

---

## 历史记录摘要

项目历史中仍保留关于 `hp ln`/`hp cp` 重命名及早期重构的记录（v0.4.x），此处保留以便查阅历史变更。

## v0.4.1
* 为 `psm cp` 子命令增加 `-r` 标志以支持递归复制目录。
* `psm cp` 支持本地文件通配符。例如：
```bash
 hp cp path/to/*.files aliat:/path/to/dest 
```

## v0.4.0
* 重命名子命令 `hp cp` 为 `hp ln`。
* 修改 hostpilot 配置字段，需手动更新配置文件；同时 `hp set` 子命令也做了相应更改。
```json
{
  "pub_key_path": "path/to/pub_key",
  "server_file_path": "/path/to/server.json",
  "ssh_client_app_path": "path/to/ssh_client_app",
  "scp_app_path": "path/to/scp_app"
}
```
* 将 `hp set -p "path/to/pub_key"` 重命名为 `hp set -k "path/to/pub_key"`。
* 新增 `hp set -a "path/to/scp_app"` 用于指定 scp 的路径。
* 新增子命令 `hp cp` 用于将文件或目录复制到远端服务器。
* 代码重构若干模块。
