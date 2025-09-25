- [English / 英文版](./README.md)

# HostPilot (HP) — 个人 SSH 服务器管理工具

HostPilot（简称 HP）是一款轻量、跨平台的命令行工具，旨在简化多台
远程 SSH 主机的日常管理。

- 使用服务器别名，避免重复输入冗长的主机名、端口和用户名。
- 使用系统 `ssh` 客户端打开交互式会话，保留原生终端体验。
- 使用内置的 SFTP 子命令 `ts` 进行高性能并发文件传输，适用于大文件
	或批量传输，并支持可配置的并发数。

HostPilot 注重可用性与自动化：交互式登录推荐使用系统 `ssh`，脚本化
传输推荐使用内置 `ts`。由于 `ts` 依赖 `ssh2`（libssh2），它不支持交互式
密码输入，建议在无人值守或自动化场景下使用公钥认证。

[Change Log](CHANGELOG.md)

---

## 快速开始

1. 列出已保存的服务器别名：

```powershell
hp ls
```

2. 新建服务器别名：

```powershell
hp new mybox root@example.com:22
```

3. 使用别名发起连接：

```powershell
hp mybox
```

4. 使用内置 SFTP (`ts`) 进行文件传输：

单文件上传示例：

```powershell
hp ts ./localfile.txt remote_alias:~/dest/path/
```

上传目录或多文件示例：

```powershell
hp ts ./folder/ ./another.txt remote_alias:~/dest/path/ -c 8
```

并发选项说明：

- `-c, --concurrency <N>`：并发 worker 数量，默认 8，最大 16（传入 0 时按 1 处理）。

示例（4 个 worker）：

```powershell
hp ts ./largefile.bin remote_alias:~/backup/ -c 4
```

更多 `ts` 使用细节请参考 `TRANSFER.md`。

文档：

- 传输细节：查看 `TRANSFER.md` 获取完整的 `ts` 示例与语义（上传、下
	载、通配、并发与失败处理）。

### 失败清单输出（JSONL）

当使用 `--output-failures <path>` 指定失败输出文件时，HP 会将失败条目以 JSON Lines（JSONL）格式追加写入 `<path>.jsonl`。每条失败一行，便于用 `jq`/Python/Node 等进行自动化处理。

示例：

```powershell
hp ts ./folder remote_alias:~/dest/ -c 8 --output-failures .\logs\transfer_failures
```

执行结束后，终端会打印最终失败清单路径：

```
失败清单已写入: .\logs\transfer_failures.jsonl
```

若加上 `--json`，结束时打印的单行 JSON 汇总还会包含 `failures_path` 字段：

```json
{
	"total_bytes": 12345,
	"elapsed_secs": 1.23,
	"files": 10,
	"session_rebuilds": 1,
	"sftp_rebuilds": 2,
	"failures": 2,
	"failures_path": ".\\logs\\transfer_failures.jsonl"
}
```

单条失败 JSON（JSONL 文件中的一行）示例：

```
{"variant":"WorkerIo","message":"local open failed: C:\\path\\to\\file2"}
```

字段与常见 variant 说明：

- variant：失败分类标识。常见取值：
	- InvalidDirection —— 使用错误：两端都为本地或都为远端。
	- UnsupportedGlobUsage —— 不支持的通配符用法；仅允许最后一段包含 `*`/`?`。
	- AliasNotFound —— 别名不存在。
	- RemoteTargetMustBeDir / LocalTargetMustBeDir —— 目标必须存在且为目录。
	- RemoteTargetParentMissing / LocalTargetParentMissing —— 目标父目录不存在。
	- CreateRemoteDirFailed / CreateLocalDirFailed —— 创建目录失败（附带 path 与 error）。
	- GlobNoMatches —— 源端 glob 无匹配项。
	- WorkerNoSession / WorkerNoSftp —— worker 无法建立会话/SFTP。
	- SftpCreateFailed —— 创建 SFTP 句柄失败。
	- SshNoAddress —— 无法解析地址。
	- SshSessionCreateFailed / SshHandshakeFailed —— 会话创建或握手失败。
	- SshAuthFailed —— 认证失败。
	- WorkerBuildSessionFailed —— worker 构建会话失败。
	- MissingLocalSource —— 本地源缺失。
	- DownloadMultipleRemoteSources —— 下载只支持单个远端源。
	- OperationFailed —— 通用操作失败。
	- WorkerIo —— 传输/IO 错误（message 中包含细节）。

- message：人类可读的信息，便于日志记录。
- alias / addr：若存在，表示失败关联的别名或地址。
- path / pattern：涉及的路径（如目标路径）或 glob 模式。
- error / detail：补充的字符串信息（嵌套错误或额外说明）。

---

## 构建与安装

从源码构建：

```powershell
# 需要安装 Rust 工具链（包含 rustc 与 cargo）
cargo build --release
# 可执行文件位于 target/release/hp
```

Windows 的打包与分发依赖 release 构建产物；如果没有官方二进制包，建
议从源码构建。

需要在 PATH 中可用的系统 `ssh` 客户端以支持交互式连接；`ts` 使用 `ssh2`
（libssh2）实现 SFTP，不支持交互式密码提示。

---

## 命令与示例

- `hp new <alias> user@host[:port]` —— 创建服务器别名
- `hp ls` —— 列出别名
- `hp <alias>` —— 使用系统 SSH 客户端发起交互式连接
- `hp ts <sources...> <target>` —— 内置 SFTP 传输（sources 可为本地路
	径或 remote alias:/path）
- `hp ln <alias>` —— 将本地公钥安装到远端 `authorized_keys`

示例：递归上传本地目录到远端：

```powershell
hp ts C:\data\project\ remote_alias:~/backup/project/ -c 6
```

示例：下载远端单个文件到本地：

```powershell
hp ts remote_alias:~/logs/sys.log C:\tmp\sys.log
```

更多 `ts` 使用细节请参考 `TRANSFER.md`。

---

## 常见问题（FAQ）

问：`ts` 支持交互式密码输入吗？

答：不支持。内置 `ts` 基于 `ssh2`（libssh2），不提供交互式密码提示功
能。建议使用 SSH 公钥认证或在环境中配置可用的私钥文件。

问：如何设置默认的 SSH 客户端或公钥路径？

答：可以使用 `hp set` 子命令，例如：

```powershell
hp set -c "C:\Windows\System32\OpenSSH\ssh.exe" -k "C:\Users\you\.ssh\id_rsa.pub"
```

问：在非 verbose 模式下，能否禁用大量的文件级进度条？

答：可以。在非 verbose 模式下，`ts` 只显示汇总进度条，或限制同时显
示的文件进度条数量（可见上限为 8），以减少终端输出噪音。

---

## 贡献

欢迎提交 Issue 或 Pull Request。在贡献前建议运行 `cargo fmt` 与 `cargo clippy`
，并保持提交描述清晰（项目偏好中文提交信息）。

建议的提交流程：

1. Fork 本仓库
2. 新建分支
3. 开发并测试
4. 提交 Pull Request 并附上清晰说明（中文）

---

## 许可

本项目采用双重授权：Apache-2.0 或 MIT。详细许可文本请见仓库根目录的
`LICENSE-APACHE` 与 `LICENSE-MIT` 文件。
