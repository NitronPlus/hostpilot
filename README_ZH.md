
![License: MIT/Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-orange.svg)

<!-- 语言切换 / Language switch -->
- [English / 英文版](./README.md)

# PSM — 个人 SSH 服务器管理工具

PSM 是一款跨平台命令行工具，旨在便捷地管理多台远程 SSH 服务器的别名、快速建立交互式连接，并通过内置的 SFTP 功能（子命令 `ts`）完成文件的上传与下载。

详细说明请参阅仓库根目录的英文版 `README.md` 或传输说明文档 `TRANSFER.md`。

---

## 快速开始

1. 列出已保存的服务器别名：

```powershell
psm ls
```

2. 新建服务器别名：

```powershell
psm new mybox root@example.com:22
```

3. 使用别名发起连接：

```powershell
psm mybox
```

4. 使用内置 SFTP (`ts`) 进行文件传输：

单文件上传示例：

```powershell
psm ts ./localfile.txt remote_alias:~/dest/path/
```

上传目录或多文件示例：

```powershell
psm ts ./folder/ ./another.txt remote_alias:~/dest/path/ -c 6
```

并发控制选项说明：

- `-c, --concurrency <N>`：并发 worker 数量，默认值为 6，最大值为 8（传入 0 时按 1 处理）。

---

## 构建与安装

从源码构建：

```powershell
# 需要安装 Rust 工具链（包含 rustc 与 cargo）
cargo build --release
# 可执行文件位于 target/release/psm
```

注意：用于交互式 SSH 连接的系统 `ssh` 客户端应位于 PATH 中；`ts` 使用 `ssh2`（libssh2）实现 SFTP，不支持交互式密码提示，建议采用 SSH 公钥认证方式。

---

## 命令与示例

- `psm new <alias> user@host[:port]` —— 创建服务器别名
- `psm ls` —— 列出所有别名
- `psm <alias>` —— 使用系统 SSH 客户端发起交互式连接
- `psm ts <sources...> <target>` —— 内置 SFTP 传输（sources 可为本地路径或 remote alias:/path）
- `psm ln <alias>` —— 将本地公钥追加到远端的 `authorized_keys`

示例：递归上传本地目录至远端：

```powershell
psm ts C:\data\project\ remote_alias:~/backup/project/ -c 6
```

示例：下载远端单个文件至本地：

```powershell
psm ts remote_alias:~/logs/sys.log C:\tmp\sys.log
```

更多 `ts` 的使用细节请参考 `TRANSFER.md`。

---

## 常见问题（FAQ）

问：`ts` 支持交互式密码输入吗？

答：不支持。内置的 `ts` 基于 `ssh2`（libssh2），不提供交互式密码提示功能。建议使用 SSH 公钥认证或在环境中配置可访问的私钥文件。

问：如何配置默认的 SSH 客户端或公钥路径？

答：可以使用 `psm set` 子命令，例如：

```powershell
psm set -c "C:\Windows\System32\OpenSSH\ssh.exe" -k "C:\Users\you\.ssh\id_rsa.pub"
```

---

问：在非 verbose 模式下，我能否禁用大量的文件级进度条？

答：可以。在非 verbose 模式下，`ts` 会显示汇总进度条，或限制同时显示的文件进度条数量，以减少终端输出噪音。

## 贡献

欢迎提交 Issue 或 Pull Request。在贡献前建议运行 `cargo fmt` 与 `cargo clippy`，并保持提交（commit）粒度合理、描述清晰。项目偏好中文提交信息。

建议的提交流程：

1. Fork 本仓库
2. 新建分支
3. 开发与测试
4. 提交 Pull Request 并附上清晰说明（中文）

---

## 许可证

本项目采用双重授权：Apache-2.0 或 MIT。详细许可文本请见仓库根目录的 `LICENSE-APACHE` 与 `LICENSE-MIT` 文件。

