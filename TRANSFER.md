# HostPilot (HP) 文件传输（ts）使用说明

本文件总结 `hp ts` 的传输规则与行为，并通过示例说明常见用法与进阶选项。

总则
- 命令格式：`hp ts <source> <target>`。
实现细节与注意事项
- 用户主目录 `~` 在远端会被预先展开一次（在主会话中）以保证一致性。
- 认证方式：优先尝试 SSH Agent；如失败再尝试 `~/.ssh/id_ed25519`、`id_rsa`、`id_ecdsa` 等常见密钥文件。
- 进度显示：采用多进度条（`indicatif::MultiProgress`），上传/下载均支持并发（默认最多 6 个并发工作线程）。
- 通配符匹配实现为简单的 `*`/`?` 匹配器；不支持 `**`、字符类或大括号扩展。需要更强的 glob 支持可以改用 `globset`。
- 错误处理：不满足目标/参数语义的情况会以友好的中文 `anyhow::Error` 返回，并在 CLI 顶层打印以便脚本/CI 使用退出码判断失败。

失败记录（failures 文件）
- 当传输过程中产生失败项（例如远端打开失败、写入失败、认证失败等），程序会把失败项打印到 stderr，并同时尝试把它们追加写入默认日志目录下的文件：
  - 默认路径：`~/.hostpilot/logs/failures_YYYYMMDD.log`（以 UTC 日期命名，格式为 `YYYYMMDD`）。
  - 写入模式：追加（append），同一日期的多次运行会追加到同一个文件末尾以便归档。
  - 文件内容示例：

```
Transfer failures (20250916):
upload failed: hdev:~/path/to/file1
local open failed: C:\\path\\to\\file2
```

- 如果无法写入该日志文件（例如权限或磁盘问题），程序会在 stderr 打印警告信息，但不会因此中止其它清理或退出流程。
- 未来可选项：支持 `--output-failures <file>` 以显式指定失败输出文件（当前实现已支持该选项，详见下面示例）。

---

## 新增/更新说明（覆盖当前实现的功能）

以下条目补充并明确了 `hp ts` 的行为，包含新近实现或参数化的功能：重试策略、退避基准、并发控制、会话复用、流式远端枚举与失败持久化。

- **重试策略 (`--retry`)**：
  - 默认对单个文件的传输失败会重试 `3` 次（可通过 `--retry <N>` 修改）。
  - 只有可重试的错误（例如网络中断、远端临时不可用、短时 I/O 错误）才会触发重试；鉴权失败、目标路径语义错误等属于不可重试错误，会立即返回失败并记录。

- **退避基准 (`--retry-backoff-ms`)**：
  - 使用线性退避策略：每次重试等待 `base_ms * (attempt_number)` 毫秒（attempt 从 1 开始计数）。
  - 默认 `base_ms = 100`（可在命令行上通过 `--retry-backoff-ms` 调整，单位为毫秒）。

- **并发与会话复用**：
  - 默认最多 `6` 个并发工作线程（在代码中为默认值，可在配置或常量中调整）。
  - 每个 worker 在其生命周期内会复用一个 SSH 会话（session）以避免频繁建立连接带来的成本。会话建立失败会触发重试逻辑。
  - 为了限制并发对远端的会话资源占用，程序使用了连接令牌桶（connection token bucket），每次建立会话前从桶中获取令牌，工作完成后归还令牌。

- **流式远端枚举**：
  - 当需要对远端目录做大规模枚举（例如数万/百万文件）时，程序采用流式 producer/consumer 模型：
    - 枚举线程（producer）边枚举边发送文件项到一个有界通道；
    - worker（consumer）以并发方式接收并处理传输任务，避免一次性将所有文件加载到内存。
  - 当 producer 的发送速率超过通道容量，producer 会短暂退避并重试发送，以避免阻塞或内存暴涨。

- **失败持久化 (`--output-failures`)**：
  - 程序会把传输失败的项追加到默认失败日志文件：`~/.hostpilot/logs/failures_YYYYMMDD.log`（UTC 日期），便于后续审计和离线重试。
  - 可通过 `--output-failures <path>` 显式写入到指定文件（支持创建父目录并以追加模式写入）。
  - 写入失败不会影响主流程的退出码，但会在 stderr 打印警告。

---

## 详细 CLI 示例（含新选项）

以下示例尽量覆盖常见与进阶用法，包含重试、退避、失败输出和并发相关的说明。

- 基本上传（本地目录到远端主目录）:

```powershell
hp ts ./build/ host:~/uploads
```

- 上传并设置最大重试次数为 5，退避基准为 200ms，并把失败写到自定义文件：

```powershell
hp ts ./build/ host:~/uploads --retry 5 --retry-backoff-ms 200 --output-failures C:\\temp\\hp_failures.log
```

- 下载远端目录到当前目录，显示进度：

```powershell
hp ts host:~/artifacts/ .
```

- 使用通配符下载仅匹配 `.log` 文件，并把失败追加到当前目录的文件：

```powershell
hp ts host:~/logs/*.log ./downloads --output-failures ./downloads/failures.log
```

- 在高延迟网络下显式增大退避基准以减少短期重试压力：

```powershell
hp ts ./build/ host:~/uploads --retry 4 --retry-backoff-ms 500
```

- 仅尝试一次（不重试）并将失败输出到默认失败日志：

```powershell
hp ts ./file.bin host:~/file.bin --retry 1
```

### 示例：批量从远端流式下载并观察失败记录

```powershell
# 从远端大目录中流式读取条目并并发下载，失败写入默认按日文件
hp ts host:~/big_dir/ ./big_downloads --retry 3 --retry-backoff-ms 200

# 之后检查失败文件（默认路径）：
Get-Content $env:USERPROFILE\\.hostpilot\\logs\\failures_$(Get-Date -Format yyyyMMdd).log | Select-String -Pattern \"Transfer failures\"
```

---

## 行为细节与常见疑问

- 为什么某些错误不重试？
  - 程序会区分“可重试”的临时错误（如网络中断、短时的远端 EIO）和“不可重试”的错误（如认证失败、目标语义不满足、目标目录不存在而命令语义要求存在）。不可重试错误会立即返回并记录到失败列表。

- 退避策略可否改为指数退避？
  - 当前实现为线性退避（可通过 `--retry-backoff-ms` 调整）。若需要指数退避，可在 util 中实现并把策略参数化；我可以帮你把这个选项加入并在 CLI 中暴露（这是未来改进项）。

- 失败文件格式及定位
  - 默认文件：`~/.hostpilot/logs/failures_YYYYMMDD.log`（UTC 日期），以纯文本追加，文件顶部会写入运行时间与标题，并逐行追加失败项。

---

如果你希望我接下来：
- 把这次文档改动提交为一个单独的 commit 并创建 PR；
- 把某些示例转换为可执行的集成测试脚本并在 CI 中验证；
- 或把线性退避扩展为可选的指数退避并在 CLI 中添加 `--retry-backoff-mode`（例如 `linear|exponential`）；

请告诉我下一步要执行的具体项。

```bash
hp ts dev hdev:~
```

- 上传文件并重命名到远端：

```bash
hp ts README.md hdev:~/README.backup
```

- 下载远端目录到当前目录（创建 ./dex）：

```bash
hp ts hdev:~/dex .
```

- 下载远端目录到已存在的本地目录（尾随 `/` 要求存在）：

```bash
mkdir -p target_dir
hp ts hdev:~/dex target_dir/
```

- 使用通配符下载匹配文件到本地目录：

```bash
hp ts hdev:~/dex/*.log ./
```

---

## 新增/更新说明（覆盖当前实现的功能）

以下条目补充并明确了 `hp ts` 的行为，包含新近实现或参数化的功能：重试策略、退避基准、并发控制、会话复用、流式远端枚举与失败持久化。

- **重试策略 (`--retry`)**：
  - 默认对单个文件的传输失败会重试 `3` 次（可通过 `--retry <N>` 修改）。
  - 只有可重试的错误（例如网络中断、远端临时不可用、短时 I/O 错误）才会触发重试；鉴权失败、目标路径语义错误等属于不可重试错误，会立即返回失败并记录。

- **退避基准 (`--retry-backoff-ms`)**：
  - 使用线性退避策略：每次重试等待 `base_ms * (attempt_number)` 毫秒（attempt 从 1 开始计数）。
  - 默认 `base_ms = 100`（可在命令行上通过 `--retry-backoff-ms` 调整，单位为毫秒）。

- **并发与会话复用**：
  - 默认最多 `6` 个并发工作线程（在代码中为默认值，可在配置或常量中调整）。
  - 每个 worker 在其生命周期内会复用一个 SSH 会话（session）以避免频繁建立连接带来的成本。会话建立失败会触发重试逻辑。
  - 为了限制并发对远端的会话资源占用，程序使用了连接令牌桶（connection token bucket），每次建立会话前从桶中获取令牌，工作完成后归还令牌。

- **流式远端枚举**：
  - 当需要对远端目录做大规模枚举（例如数万/百万文件）时，程序采用流式 producer/consumer 模型：
    - 枚举线程（producer）边枚举边发送文件项到一个有界通道；
    - worker（consumer）以并发方式接收并处理传输任务，避免一次性将所有文件加载到内存。
  - 当 producer 的发送速率超过通道容量，producer 会短暂退避并重试发送，以避免阻塞或内存暴涨。

- **失败持久化 (`--output-failures`)**：
  - 程序会把传输失败的项追加到默认失败日志文件：`~/.hostpilot/logs/failures_YYYYMMDD.log`（UTC 日期），便于后续审计和离线重试。
  - 可通过 `--output-failures <path>` 显式写入到指定文件（支持创建父目录并以追加模式写入）。
  - 写入失败不会影响主流程的退出码，但会在 stderr 打印警告。

---

## 详细 CLI 示例（含新选项）

以下示例尽量覆盖常见与进阶用法，包含重试、退避、失败输出和并发相关的说明。

- 基本上传（本地目录到远端主目录）:

```powershell
hp ts ./build/ host:~/uploads
```

- 上传并设置最大重试次数为 5，退避基准为 200ms，并把失败写到自定义文件：

```powershell
hp ts ./build/ host:~/uploads --retry 5 --retry-backoff-ms 200 --output-failures C:\temp\hp_failures.log
```

- 下载远端目录到当前目录，限制默认并发并打印进度：

```powershell
hp ts host:~/artifacts/ .
```

- 使用通配符下载仅匹配 `.log` 文件，并把失败追加到当前目录的文件：

```powershell
hp ts host:~/logs/*.log ./downloads --output-failures ./downloads/failures.log
```

- 在高延迟网络下显式减小退避基准以减少短期重试压力：

```powershell
hp ts ./build/ host:~/uploads --retry 4 --retry-backoff-ms 500
```

- 仅尝试一次（不重试）并将失败输出到默认失败日志：

```powershell
hp ts ./file.bin host:~/file.bin --retry 1
```

### 示例：批量从远端流式下载并观察失败记录

```powershell
# 从远端大目录中流式读取条目并并发下载，失败写入默认按日文件
hp ts host:~/big_dir/ ./big_downloads --retry 3 --retry-backoff-ms 200

# 之后检查失败文件（默认路径）：
Get-Content $env:USERPROFILE\.hostpilot\logs\failures_$(Get-Date -Format yyyyMMdd).log | Select-String -Pattern "Transfer failures"
```

---

## 行为细节与常见疑问

- 为什么某些错误不重试？
  - 程序会区分“可重试”的临时错误（如网络中断、短时的远端 EIO）和“不可重试”的错误（如认证失败、目标语义不满足、目标目录不存在而命令语义要求存在）。不可重试错误会立即返回并记录到失败列表。

- 退避策略可否改为指数退避？
  - 当前实现为线性退避（可通过 `--retry-backoff-ms` 调整）。若需要指数退避，可在 util 中实现并把策略参数化；我可以帮你把这个选项加入并在 CLI 中暴露（这是未来改进项）。

- 失败文件格式及定位
  - 默认文件：`~/.hostpilot/logs/failures_YYYYMMDD.log`（UTC 日期），以纯文本追加，文件顶部会写入运行时间与标题，并逐行追加失败项。
