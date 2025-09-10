````markdown
# TS 手动测试用例说明（TESTING_TS.md）

目的：手动验证 `psm ts` 的行为是否符合 `TRANSFER.md` 规范（包括尾随 `/` 语义、通配符行为、目标路径处理、并发与进度）。

准备工作（前置条件）
- 准备两台环境：本地测试机器（执行 `psm`）和远端测试服务器（可通过 alias `hdev` 访问，需在 `config` 中配置别名）。
- 远端需允许基于密钥或 agent 的 SSH 登录，并可用 SFTP。确保远端测试目录可读写。
- 在仓库根确保已编译当前代码：

```powershell
cargo build
```

- 在本地创建测试目录结构（示例）：

```powershell
mkdir -p testdata/local/dir1
set-content -Path testdata/local/dir1/file1.txt -Value "file1"
set-content -Path testdata/local/dir1/file2.log -Value "file2"
set-content -Path testdata/local/file_root.txt -Value "root"
```

远端准备（示例命令，视远端 shell 为 bash）：

```bash
ssh hdev 'rm -rf ~/psm_test || true; mkdir -p ~/psm_test/dex/dex2; echo hello > ~/psm_test/dex/file_a.txt; echo log1 > ~/psm_test/dex/file.log; echo nested > ~/psm_test/dex/dex2/nested.txt'
```

测试用例清单（对应 `TRANSFER.md` 的 1-11 场景）

每个测试用例包含：命令、预期行为、验证步骤、清理建议。

用例 1: 本地目录上传到远端主目录
- 命令：`psm ts testdata/local hdev:~`
- 预期：远端 `~/testdata/local/` 被创建，包含 `dir1/file1.txt`、`dir1/file2.log`、`file_root.txt`。
- 验证：ssh 到远端查看 `ls -R ~/testdata/local`。
- 清理：`ssh hdev 'rm -rf ~/testdata/local'`

用例 2: 本地目录上传到远端主目录（目标带尾 `/`）
- 命令：`psm ts testdata/local hdev:~/`
- 预期：行为与用例1 相同。
- 验证与清理同上。

用例 3: 本地目录上传到远端并重命名目标（无尾 `/`）
- 命令：`psm ts testdata/local hdev:~/psm_upload`
- 预期：远端 `~/psm_upload/` 存在，包含 `local` 内容（如果希望 `psm_upload/local` 出现请使用 `~/psm_upload/`）。
- 验证：`ssh hdev 'ls -R ~/psm_upload'`
- 清理：`ssh hdev 'rm -rf ~/psm_upload'`

用例 4: 本地目录上传到远端已存在目录（目标带 `/`）
- 前置：`ssh hdev 'mkdir -p ~/psm_parent'`
- 命令：`psm ts testdata/local hdev:~/psm_parent/`
- 预期：远端 `~/psm_parent/local/...` 被创建（作为子目录）。
- 验证：`ssh hdev 'ls -R ~/psm_parent'`
- 清理：`ssh hdev 'rm -rf ~/psm_parent'`

用例 5: 远端目录下载到本地（目标为 dev，自动创建）
- 命令：`psm ts hdev:~/psm_test/dex testdata/dev`
- 预期：本地 `testdata/dev/` 被创建并包含远端 `dex` 的文件。
- 验证：`ls -R testdata/dev`
- 清理（本地）：`rm -rf testdata/dev`

用例 6: 远端目录下載到本地已存在目录（目标帶 `/` 要求存在）
- 前置：`mkdir -p testdata/existing_dev`
- 命令：`psm ts hdev:~/psm_test/dex testdata/existing_dev/`
- 预期：本地 `testdata/existing_dev/dex/` 被创建并包含远端內容。
- 验證：`ls -R testdata/existing_dev`
- 清理：`rm -rf testdata/existing_dev`

用例 7: 远端子目录下载到本地（自动创建父目录）
- 命令：`psm ts hdev:~/psm_test/dex/dex2 testdata/dev2`
- 预期：创建 `testdata/dev2/`（如果不存在），并把 `dex2/` 的内容放入 `testdata/dev2/`。
- 验证：`ls -R testdata/dev2`
- 清理：`rm -rf testdata/dev2`

用例 8: 远端子目录下載到本地已存在父目录（目标帶 `/`）
- 前置：`mkdir -p testdata/parent`
- 命令：`psm ts hdev:~/psm_test/dex/dex2 testdata/parent/`
- 预期：`testdata/parent/dex2/` 被创建并包含內容。
- 验证：`ls -R testdata/parent`
- 清理：`rm -rf testdata/parent`

用例 9: 远端目录下载到当前目录（使用 `.`）
- 命令：`cd testdata; psm ts hdev:~/psm_test/dex .`
- 预期：在 `testdata/` 下创建 `dex/` 并包含遠端內容。
- 验证：`ls -R testdata/dex`
- 清理：`rm -rf testdata/dex`

用例 10: 使用通配符僅匹配文件下載到当前目录（非遞歸）
- 命令：`psm ts hdev:~/psm_test/dex/*.log testdata/logs/`
- 预期：僅匹配 `*.log` 的文件被下載到 `testdata/logs/`，不會進入子目錄。
- 验证：`ls testdata/logs`
- 清理：`rm -rf testdata/logs`

用例 11: 使用通配符下載匹配文件到指定目錄（自動創建）
- 命令：`psm ts hdev:~/psm_test/dex/*.txt testdata/matched/`
- 预期：匹配的 `.txt` 文件被下載到 `testdata/matched/`。
- 验证：`ls testdata/matched`
- 清理：`rm -rf testdata/matched`

边界与错误用例
- 错误用例 A：目标以 `/` 结尾但不存在（例如 `psm ts localdir hdev:~/noexist/`）应返回错误。
- 错误用例 B：源以 `/` 结尾但实际上是文件（例如 `psm ts somefile/ hdev:~`）应返回错误。
- 错误用例 C：远端通配符无匹配文件，应回退为单文件行为或返回友好错误（按实现观察）。

额外检查
- 并发行为：在较大的目录上传/下载时，观察 CPU/网络与是否存在失败重试或连接问题。
- 进度条：检查总进度与单文件进度是否合理，完成后是否清理单文件进度条的显示行。

清理脚本示例（本地）

```powershell
rm -rf testdata
```

清理脚本示例（远端）

```bash
ssh hdev 'rm -rf ~/psm_test ~/testdata ~/psm_upload ~/psm_parent'
```

---

运行小贴士
- 若遇到权限或连接问题，请先单独使用 `ssh hdev` 确认连通性与权限。
- 若 `psm ts` 报错信息中含 `anyhow` 文本，请把错误文本记录下来并提供给我，我可以帮助定位。

结束。

````
# TS 手动测试用例说明（TESTING_TS.md）

目的：手动验证 `psm ts` 的行为是否符合 `TRANSFER.md` 规范（包括尾随 `/` 语义、通配符行为、目标路径处理、并发与进度）。

测试用例示例：
- 命令：`psm ts testdata/local hdev:~`
- 命令：`psm ts testdata/local hdev:~/`
- 命令：`psm ts testdata/local hdev:~/psm_upload`
- 命令：`psm ts testdata/local hdev:~/psm_parent/`
- 命令：`psm ts hdev:~/psm_test/dex testdata/dev`
- 命令：`psm ts hdev:~/psm_test/dex testdata/existing_dev/`
- 命令：`psm ts hdev:~/psm_test/dex/dex2 testdata/dev2`
- 命令：`psm ts hdev:~/psm_test/dex/dex2 testdata/parent/`
- 命令：`cd testdata; psm ts hdev:~/psm_test/dex .`
- 命令：`psm ts hdev:~/psm_test/dex/*.log testdata/logs/`
- 命令：`psm ts hdev:~/psm_test/dex/*.txt testdata/matched/`

错误用例示例：
- 错误用例 A：目标以 `/` 结尾但不存在（例如 `psm ts localdir hdev:~/noexist/`）应返回错误。
- 错误用例 B：源以 `/` 结尾但实际上是文件（例如 `psm ts somefile/ hdev:~`）应返回错误。

- 若 `psm ts` 报错信息中含 `anyhow` 文本，请把错误文本记录下来并提供给我，我可以帮助定位。

注：本文件名从 `TESTING_STSF.md` 重命名为 `TESTING_TS.md`，内容中所有示例均改为 `psm ts`。
