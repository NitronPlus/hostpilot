TRANSFER 使用说明（中文）
=========================

概述
----
`hp ts` 是 hostpilot 的文件/目录传输子命令，用于在本地与远端（通过 SSH/SFTP）之间上传和下载文件与目录，支持并发、重试和可观测性（tracing）。本文件为完整中文参考，包含语义定义、示例、调试建议与实现细节注释，便于开发者和运维使用。

快速示例
---------

- 下载远端目录到本地目录（并发 4，重试 1 次，详细输出）：

```
hp ts hdev:~/project/dist dist -c 4 -r 1 -v
```

- 上传本地目录到远端路径（并发 2，重试 2 次）：

```
hp ts dist hdev:~/project/dist -c 2 -r 2
```

- 调试（打开详细 tracing 并将 stdout/stderr 保存为日志，PowerShell）：

```
$env:RUST_LOG="debug"; hp ts hdev:~/project/dist dist -c 4 -r 1 -v 2>&1 | Tee-Object hp-ts-run.log
```

安静模式与 CI 友好输出

对于 CI/脚本建议使用 `--quiet` 配合 `--json`：在安静模式下，HostPilot 会抑制人类可读的进度和汇总输出，但若指定 `--json` 则会在结束时打印一行 JSON 汇总，包含 `total_bytes`、`elapsed_secs`、`files`、`failures` 等字段以及（当存在失败项时）`failures_path` 指向默认写入的 `failures.jsonl`。

边界情况与降级策略

- `--quiet` 的作用边界：在安静模式下，HostPilot 会抑制面向人的进度条和摘要行（这些通常打印到 stdout）。
  但面向机器的输出（当使用 `--json` 时的单行 JSON 汇总）仍然会输出到 stdout。此外，对于关键性的运行时问题（例如无法写入
  `failures.jsonl`），HostPilot 会在 stderr 上输出一条简短警告，便于 CI 或运维工具捕获并上报。

- failures.jsonl 写入语义（CI 中的期望行为）：
  - 程序尝试写入到规范日志目录（通常为 `~/.hostpilot/logs/failures.jsonl`）。若写入成功，`--json` 的汇总中将包含 `failures_path`。
  - 若无法创建或写入该目录（例如权限问题），程序会尝试创建目录；若仍然失败，程序不会抛出不同的退出码，但会向 stderr 打印一行
    可被机器检测的简短警告，说明写入失败的原因（例如权限或磁盘空间）。在这种情况下，汇总可能仍然报告 `failures` 数量，但不会
    返回 `failures_path` 字段。
  - HostPilot 不会在写入失败时更改整体的退出码：stderr 警告是为了可观察性而存在。如果 CI 需要在写入失败时失败构建，请在 CI
    脚本中检测 stderr 并将其视为失败条件。

- 在 CI 中同时捕获 stdout JSON 和 stderr 警告的示例：
  - PowerShell：

```powershell
# Capture JSON summary on stdout and warnings on stderr
$summary = & hp ts ./localdir remote_alias:~/dest --quiet --json 2>hp_ts.err
if ($summary) {
    $obj = $summary | ConvertFrom-Json
    Write-Output "Files: $($obj.files)  Bytes: $($obj.total_bytes)  Failures: $($obj.failures)"
    if ($obj.failures -gt 0 -and $obj.failures_path) {
        Get-Content $obj.failures_path | ForEach-Object { $_ | ConvertFrom-Json } | Out-File failures_parsed.json
        Write-Output "Parsed failures written to failures_parsed.json"
    } elseif ($obj.failures -gt 0) {
        Write-Output "Failures present but no failures_path was written. See hp_ts.err for write warnings."
        Get-Content hp_ts.err | Write-Output
    }
} else {
    if (Test-Path hp_ts.err) { Get-Content hp_ts.err | Write-Output }
}
```

  - Bash / sh：

```sh
# Capture JSON summary on stdout and warnings on stderr
summary=$(hp ts ./localdir remote_alias:~/dest --quiet --json 2>hp_ts.err)
echo "$summary" | jq '.'
fail_path=$(echo "$summary" | jq -r '.failures_path // empty')
if [ -n "$fail_path" ] && [ -f "$fail_path" ]; then
  jq -s '.' "$fail_path" > failures.json
  echo "Parsed failures written to: failures.json"
elif [ -s hp_ts.err ]; then
  echo "Failures present but no failures_path was written; see stderr warnings:"
  cat hp_ts.err
fi
```

注意事项：

- `failures.jsonl` 为 append-only 文件，可能包含来自之前运行的条目。在 CI 中通常需要将其转换为 JSON 数组（例如 `jq -s '.'）
  以便于上传成 artifact 或做进一步处理。
- 如果需要每次运行产出确定性的 failures 文件，请在 CI 作业中对 `~/.hostpilot/logs/failures.jsonl` 做 rotate/重命名或在脚本中筛选
  出某个时间窗口内的条目；当前 HostPilot 不会自动轮转该文件。

PowerShell（在 CI 中捕获 summary 并解析 failures）：

```powershell
# Run transfer in quiet+json mode and capture the single-line JSON summary
$summary = hp ts ./localdir hdev:~/dest --quiet --json
if ($summary) {
    $obj = $summary | ConvertFrom-Json
    Write-Output "Files: $($obj.files)  Bytes: $($obj.total_bytes)  Failures: $($obj.failures)"
    if ($obj.failures -gt 0 -and $obj.failures_path) {
        Get-Content $obj.failures_path | ForEach-Object { $_ | ConvertFrom-Json } | Out-File failures_parsed.json
        Write-Output "Parsed failures written to failures_parsed.json"
    }
}
```

Bash / sh（在 CI 中捕获 summary 并解析 failures）：

```sh
# Run transfer in quiet+json mode and capture the single-line JSON summary
summary=$(hp ts ./localdir hdev:~/dest --quiet --json)
echo "$summary" | jq '.'
fail_path=$(echo "$summary" | jq -r '.failures_path')
if [ -n "$fail_path" ] && [ -f "$fail_path" ]; then
  jq -s '.' "$fail_path" > failures.json
  echo "Parsed failures written to: failures.json"
fi
```

上面的示例展示了一个典型的 CI 流程：以静默模式运行并获取结构化 summary，若有失败则读取 `failures.jsonl` 将其转换为 JSON 数组以便进一步处理或上报。


命令语法
--------

基本形式：

```
hp ts <source> <target> [options]
```

- 若 `<source>` 或 `<target>` 包含 `:`，则含 `:` 的参数视为远端别名（alias）加远端路径，例如 `hdev:~/path`。
- 否则该参数为本地路径。

常用选项（示例）：

- `-c, --concurrency <N>`: 并发 worker 数量（默认值见 CLI 帮助）。
- `-r, --retries <N>`: 单个文件传输的重试次数。
- `-v, --verbose`: 打印更多运行信息（与 `RUST_LOG` 结合使用可获得更详细日志）。

并发、缓冲与可见进度条上限
---------------------------

- `-c, --concurrency <N>`：控制并发 worker 数量。CLI 默认值为 8，最大上限为 16（传入 0 会被视为 1）。该值影响实际并发 worker 数量，但并不会限制可见的文件级进度条数量。
- `-f, --buf-mib <N>`：每个 worker 的 IO 缓冲大小（以 MiB 为单位）。默认 `1`，允许范围 `1..=8`，可用于在不同网络/磁盘环境下做性能调优。
- 进度条可见上限：为了减少终端噪音，在非 verbose 模式下同时可见的文件级进度条数量被限制为 8（上传与下载双方皆相同）。该限制仅影响显示，不会影响实际并发或传输速率。
核心语义（详细）
-----------------

以下规则描述 `hp ts` 在边界情形下的行为，这些规则与实现保持一致：

1. 目标归属判定
   - 参数中带有 `:`（例如 `alias:path`）的一端被视为远端，另一端为本地。

2. 相对本地目标自动前缀
   - 对于本地目标，如果它是相对路径且没有以 `./` 或 `../` 开头，CLI 会在内部把它解释为 `./<target>`，以避免不小心写入根目录或误解路径。

3. 远端为目录时的行为
   - 如果远端源是目录，且本地目标不存在或为目录，`hp ts` 会在本地创建目标目录（如 `./dist`），并将远端目录下的文件按相对路径写入本地目标（例如远端 `dist/sub/a.txt` -> 本地 `dist/sub/a.txt`）。

4. 远端为单个文件时的行为
   - 如果远端源是单个文件，且本地目标是目录，则文件写入该目录并保留原名；如果本地目标是文件路径，则写成该文件（覆盖或创建）。

5. 原子写入与临时文件
   - 为避免产生 0 字节或损坏文件，传输过程中会先写入临时文件（如 `file.hp.part.<pid>`），写入完成后调用 `sync_all` 并重命名到目标文件名。若传输失败，临时文件会被清理或保留以便排查，但不会覆盖目标文件。

6. 并发与会话复用
  - 有关并发控制、会话复用与连接令牌等实现细节，请参见下文“新增/更新说明”中的 **并发与会话复用** 段落（包含默认并发值、session 重用及令牌桶行为）。

7. 认证策略（不依赖 ssh-agent）
   - 为提高跨平台稳定性（特别是在 Windows 环境），`hp ts` 不再依赖系统 `ssh-agent`。默认尝试用户主目录下的常见私钥文件（例如 `~/.ssh/id_ed25519`、`id_rsa` 等）。如果需要 agent，也可以在外部运行 agent 并手动配置，但 agent 的失败不会导致传输直接中断。

8. 重试与错误上下文
   - 支持为单个文件传输配置重试次数。错误将以带上下文的 `anyhow::Error` 返回，CLI 会在日志中打印文件路径、错误类型与尝试次数，便于后续重试或手动修复。

并发、缓冲与可见进度条上限
---------------------------

- `-c, --concurrency <N>`：控制并发 worker 数量。CLI 默认值为 8，最大上限为 16（传入 0 会被视为 1）。该值影响实际并发 worker 数量，但并不会限制可见的文件级进度条数量。
- `-f, --buf-mib <N>`：每个 worker 的 IO 缓冲大小（以 MiB 为单位）。默认 `1`，允许范围 `1..=8`，可用于在不同网络/磁盘环境下做性能调优。
- 进度条可见上限：为了减少终端噪音，在非 verbose 模式下同时可见的文件级进度条数量被限制为 8（上传与下载双方皆相同）。该限制仅影响显示，不会影响实际并发或传输速率。
实现细节（对开发者）
---------------------

- 缓冲区与内存：为减少重复分配，worker 复用固定大小的缓冲区（例如 1 MiB），在高并发或大文件场景能显著降低内存抖动。
- 进度显示：使用 `indicatif::MultiProgress` 为每个文件展示进度条，样式在创建时共享并为每个文件克隆以避免大量重复分配。
- 错误与日志：使用 `tracing` 记录关键事件（建立连接、认证方式、重试、写入完成/失败），配合 `RUST_LOG` 环境变量可获取本地诊断日志。

常见问题与排查
----------------

1) 我看到 0 字节文件或不完整文件
   - 请在运行命令时带 `-v` 或设置 `RUST_LOG=debug` 并查看日志；现在实现使用临时文件 + sync + 重命名，故理论上不会留下 0 字节的目标文件。若出现，请提供命令输出日志（上面的 `Tee-Object` 示例）。

2) 连接/认证失败，提示 agent 错误
  - 当前版本不依赖 `ssh-agent`；实现会直接尝试常见私钥文件（例如 `~/.ssh/id_ed25519`、`id_rsa` 等）。如果认证失败，请确认私钥文件可用并检查日志（`RUST_LOG=debug`）。

3) 本地目标成为单一文件（而不是目录）
   - 当你传入的本地目标为 `dist`（未以 `/` 或 `./` 开头）且远端是目录，CLI 会将其视为 `./dist` 并创建目录。如果你遇到本地 `dist` 被当作文件的情况，请提供运行时日志，或使用 `./dist/` 明确指定为目录。

命令行参数与兼容性
--------------------

本项目不会在文档中暗示或依赖未实现的运行时参数来改变核心传输语义。当前支持的 CLI 选项（例如 `-c/--concurrency`、`-r/--retries`、`-v/--verbose` 等）仍然有效并受实现支持。任何对传输语义的更改会通过代码实现并在变更说明中明确，而不会在使用说明中预先假定不存在的参数。

示例场景（常见）
-----------------

1. 从远端目录批量下载到本地目录（保持目录结构）：

```
hp ts hdev:~/project/dist dist -c 8 -r 2 -v
```

2. 将本地构建产物上传到远端目录（跳过 .git）：

```
hp ts ./dist hdev:~/deploy/dist -c 4 -r 1
```

3. 在 CI 中推荐的调试命令（保存日志）：

```
# PowerShell
$env:RUST_LOG="debug"; hp ts hdev:~/project/dist dist -c 4 -r 1 -v 2>&1 | Tee-Object hp-ts-run.log

# Unix shell
RUST_LOG=debug hp ts hdev:~/project/dist dist -c 4 -r 1 -v 2>&1 | tee hp-ts-run.log
```

反馈与贡献
---------------

如果你希望我：

-- 将 `TRANSFER.md` 中的行为改为更严格的实现（例如永远拒绝自动创建本地目录），请说明希望的默认语义，我可以据此修改实现（该修改需要代码变更并通过测试）。
-- 我可以把这份文档自动添加到 `README_ZH.md` 的索引处并提交为单独的 commit/PR。

限制与兼容性说明
------------------

- **单远端限制**：本应用仅支持命令中**一端为远端**（格式 `alias:path`），另一端为本地路径；不支持同时指定多个远端作为目标或源，未来也不会支持多远端同时传输的场景。
  - **glob 限制**：不支持复杂递归 glob（例如 `**` 或 `source/*/` 等）。支持的通配符为基础的 `*` 和 `?`，且 glob 展开仅在源端执行（远端在远端展开，本地在本地展开）。当前实现不支持 `-R/--recursive` 选项；当需要递归复制目录内容时，请显式指定源目录并使用尾部 `/` 表示复制目录内容。

  - **允许 / 禁止示例（明确规则）**：
    - 允许：只在路径最后一个段（basename）使用 `*` 或 `?`，例如：
      - `hdev:~/logs/*.log` （匹配远端 `logs` 目录下所有 `.log` 文件）
      - `hdev:~/data/file-?.txt` （匹配 `file-a.txt` / `file-1.txt` 等单字符变体）
      - `./dist/*.wasm` （本地 glob，在本地展开）

    - 禁止或会报错：在中间路径段使用通配符或使用递归 glob：
      - `hdev:~/src/*/file.txt` （中间段 `*` 不被允许）
      - `hdev:~/src/**/file.txt` （递归 `**` 不被允许）
      - `hdev:~/*/secret/*.key` （在非尾部段包含通配符会被拒绝）

    - 目标（`<target>`）不得包含通配符：
      - 错误示例：`hp ts hdev:~/logs/*.log ./out/*.log`（目标包含 `*`，会被拒绝）

    - 行为说明：如果源端 glob 未匹配到任何文件，命令会返回错误并退出；若需要匹配目录中的所有内容，请指明目录（尾部 `/`）而非尝试中间段通配符。

补充短语（重要提醒）:

- “无尾部 / 的目录被视为单一条目；带尾部 / 表示复制目录内容。”
- “glob 若无匹配项则视为错误并退出（请确认 glob 模式与路径是否正确）。”
- “目标创建：当目标不存在且其父目录存在时会创建目标目录（等同 mkdir <target>）。在上传场景中，程序会确保远端父目录链存在并按需创建（等同 mkdir -p），以便将文件写入目标路径；若父目录无法创建或存在同名文件则会返回错误并记录。”


许可证与版权
----------------

本项目遵循仓库根目录的 LICENSE（MIT/Apache）。文档由仓库维护者编辑与发布。
# HostPilot (HP) 文件传输（ts）使用说明

本文件总结 `hp ts` 的传输规则与行为，并通过示例说明常见用法与进阶选项。

总则
- 命令格式：`hp ts <source> <target>`。
实现细节与注意事项
- 用户主目录 `~` 在远端会被预先展开一次（在主会话中）以保证一致性。
- 认证方式（不依赖 `ssh-agent`）：为提高跨平台稳定性，当前实现**不依赖**系统 `ssh-agent`。默认直接尝试用户主目录下的常见私钥文件（例如 `~/.ssh/id_ed25519`、`id_rsa`、`id_ecdsa` 等）。如果需要使用 agent，可以在外部启动并配置，但 agent 的不可用或失败不会导致传输直接中断；更多调试信息请参见日志（`RUST_LOG=debug`）。
- 进度显示：采用多进度条（`indicatif::MultiProgress`），上传/下载均支持并发（默认最多 6 个并发工作线程）。
- 通配符匹配实现为简单的 `*`/`?` 匹配器；不支持 `**`、字符类或大括号扩展。需要更强的 glob 支持可以改用 `globset`。
- 错误处理：不满足目标/参数语义的情况会以友好的中文 `anyhow::Error` 返回，并在 CLI 顶层打印以便脚本/CI 使用退出码判断失败。

失败记录（JSONL）
当传输过程中产生失败项（例如远端打开失败、写入失败、认证失败等），程序会把失败项打印到 stderr，并支持将失败清单以 JSON Lines（JSONL）格式写入文件：
  - 将失败清单以 JSON Lines（JSONL）格式追加写入 HostPilot 的 canonical 日志目录：`~/.hostpilot/logs/`，默认文件名为 `failures.jsonl`（固定名，追加写入）；程序在运行结束时会打印写入的路径以便检索与自动化处理。
  - 条目格式：每个失败项为一个 JSON 对象，包含 `variant`、错误相关字段以及 `message`；示例：

```
{"variant":"SshAuthFailed","addr":"hdev","message":"authentication failed"}
{"variant":"WorkerIo","message":"local open failed: C:\\path\\to\\file2"}
```

  - 写入成功时，命令结束会在控制台打印 JSONL 文件路径；若使用 `--json`，汇总 JSON 也会包含 `failures_path` 字段（字符串路径）。

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

- **失败持久化（默认行为）**：
  - 程序会把传输失败的项追加到默认失败日志文件：`~/.hostpilot/logs/failures.jsonl`（固定名，追加写入），便于后续审计和离线重试。文件为 JSON Lines 格式。CLI 不再支持 `--output-failures` 来指定替代路径。
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
hp ts ./build/ host:~/uploads --retry 5 --retry-backoff-ms 200
```

- 下载远端目录到当前目录，限制默认并发并打印进度：

```powershell
hp ts host:~/artifacts/ .
```

- 使用通配符下载仅匹配 `.log` 文件，并把失败追加到当前目录的文件：

```powershell
hp ts host:~/logs/*.log ./downloads
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
Get-Content $env:USERPROFILE\\.hostpilot\\logs\\failures.jsonl | Select-String -Pattern \"Transfer failures\"
```

---

## 在脚本 / CI 中查找失败文件

在自动化脚本或 CI 中，你通常需要以可编程方式定位并读取刚写入的失败 JSONL。下面是两个常见平台的示例。

- PowerShell (Windows, CI runner)：

```powershell
# 默认失败文件路径（append-only）
$failPath = Join-Path $env:USERPROFILE (".hostpilot\\logs\\failures.jsonl")
if (Test-Path $failPath) {
  # 只输出包含失败项的行到汇总文件
  Get-Content $failPath | Select-String -Pattern '"variant"' | Out-File -FilePath "./failures_summary.txt"
  Write-Output "Failures written to: $failPath"
} else {
  Write-Output "No failures file found at $failPath"
}
```

- Bash / sh (Unix-like CI)：

```sh
# 使用 UTC 日期（与 HostPilot 写入时的日期一致）
fail_path="$HOME/.hostpilot/logs/failures.jsonl"
if [ -f "$fail_path" ]; then
  # 将包含失败记录的行筛选到工作目录下的汇总文件
  grep '"variant"' "$fail_path" > failures_summary.txt
  echo "Failures written to: $fail_path"
else
  echo "No failures file found at $fail_path"
fi
```

这些示例演示如何在脚本中自动检测并收集当日的失败文件；根据需要可改为用 glob 搜索最近的文件，或在运行结束后同时抓取 `~/.hostpilot/logs/debug.log` 做进一步关联。

## 行为细节与常见疑问

- 为什么某些错误不重试？
  - 程序会区分“可重试”的临时错误（如网络中断、短时的远端 EIO）和“不可重试”的错误（如认证失败、目标语义不满足、目标目录不存在而命令语义要求存在）。不可重试错误会立即返回并记录到失败列表。

- 退避策略可否改为指数退避？
  - 当前实现为线性退避（可通过 `--retry-backoff-ms` 调整）。若需要指数退避，可在 util 中实现并把策略参数化；我可以帮你把这个选项加入并在 CLI 中暴露（这是未来改进项）。

- 失败文件格式及定位
  - 默认文件：`~/.hostpilot/logs/failures.jsonl`（append-only），以 JSON Lines 格式追加；每行是一个独立的 JSON 对象，便于脚本或工具逐行解析。

  机器可读规则（开发者参考）
  -------------------------

  以下为 `hp ts` 传输语义的“机器可读”摘要，便于在其他实现/设备上对齐行为。该结构非严格 JSON Schema，但可直接解析为 JSON 对象列表。

  ```json
  [
    { "id": "A", "name": "TargetSideDetection", "rule": "Exactly one endpoint is remote (alias:path); the other is local.",
      "validate": { "remote_sides": 1 } },

    { "id": "B", "name": "LocalRelativeTargetNormalization", "rule": "Local relative targets without ./ or ../ are interpreted under current directory.",
      "examples": [ {"in": "dist", "norm": "./dist"} ] },

    { "id": "C", "name": "DirectorySourceWriteToTargetDir", "rule": "When source is a directory, copy directory contents recursively (no top-level container).",
      "notes": ["src and src/ are equivalent for directories"] },

    { "id": "D", "name": "SingleFileSourceBehavior", "rule": "When source is a single file and target is a directory, place under target keeping basename; when target is a file path, write to that file." },

    { "id": "E", "name": "AtomicWrites", "rule": "Use temp file -> sync -> rename to avoid partial files." },

    { "id": "F", "name": "ConcurrencyAndReuse", "rule": "Bounded workers with per-worker session and buffer reuse; token bucket for connections." },

    { "id": "G", "name": "AuthWithoutAgent", "rule": "Do not depend on ssh-agent; try common key files under ~/.ssh/ first." },

    { "id": "H", "name": "RetryPolicy", "rule": "Retry per-file on transient errors up to configured attempts; non-retriable errors fail fast." },

    { "id": "I", "name": "HomeTildeExpansionRemote", "rule": "Remote ~ and ~/ expand to $HOME before path operations." },

    { "id": "J", "name": "GlobSemantics", "rule": "Only * and ? are supported; glob expands only at the source side; directories matched by glob are NOT recursed.",
      "constraints": { "forbid": ["**"], "position": "basename-only" },
      "onEmpty": "error" },

    { "id": "K", "name": "LocalDotHandling", "rule": "Local target '.' or './' normalize to CWD path." },

    { "id": "L", "name": "DirectionConstraints", "rule": "Upload (local->remote) allows multiple sources; Download (remote->local) allows exactly one remote source." },

    { "id": "M", "name": "TargetWithSlash", "rule": "Target ending with '/' must exist and be a directory; otherwise error.",
      "side": ["local", "remote"] },

    { "id": "N", "name": "TargetWithoutSlashCreation", "rule": "When target (without trailing '/') does not exist, create a single-level directory if parent exists; do not create parents recursively.",
      "parentRequired": true, "mkdirP": false },

    { "id": "O", "name": "SingleFileToExistingFileTarget", "rule": "If target exists and is a file and there is exactly one file source, write to that exact file path; otherwise require directory target." }
  ]
  ```

  实现备注：
  - 上述条目覆盖了“源目录递归复制内容（不含容器）”、“glob 非递归”、“目标单层创建”、以及“远端 ~ 展开”等关键语义。实际实现以 `src/transfer.rs` 为准，如需扩展（例如指数退避、忽略模式），可在此列表新增条目并在代码中对齐。

---


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

更详细示例集（按场景）
-------------------

1) 上传：把本地构建产物上传到远端目录（跳过 .git），并把失败写到指定文件：

```powershell
hp ts ./dist/ hdev:~/deploy/dist --retry 3 --retry-backoff-ms 200
```

说明：末尾的 `/` 表示复制目录内部内容；如果远端目录不存在且其父目录存在，则会创建目标目录；若父目录不存在则会报错。

2) 按后缀批量下载（basename-level glob，允许）：

```bash
hp ts hdev:~/logs/*.log ./downloads
```

说明：`*.log` 只在远端 `logs` 目录的 basename 上展开；若没有任何匹配项，命令会返回错误。

3) 禁止的模式（会被拒绝）：

```bash
# 以下命令会被拒绝，因中间段包含通配符
hp ts hdev:~/src/*/file.txt ./

# 递归 glob 也不被支持
hp ts hdev:~/src/**/file.txt ./
```

4) 递归复制目录（正确方式）：

```bash
# 指定源为目录（尾部 `/`），复制目录内容到本地目录
hp ts hdev:~/big_dir/ ./big_downloads --retry 3
```

5) 原子写入示意（发生在下载时）

 - 运行命令时，程序会先写入临时文件（例如 `file.hp.part.<pid>`），写入完成后调用 `sync_all()` 并重命名为最终文件名。
 - 如果任务失败，临时文件会被清理或保留用于排查，但不会替换目标文件。

6) 重试与退避示例：

```bash
hp ts ./build/ host:~/uploads --retry 5 --retry-backoff-ms 500
```

说明：每次重试按线性退避（`base_ms * attempt_number`）等待；失败项会被持久化到 HostPilot 的日志目录以便离线审计与自动化处理。

7) Windows 路径示例（PowerShell）：

```powershell
hp ts C:\path\to\file.bin hdev:~/file.bin --retry 1
```

8) 目标包含通配符会被拒绝：

```bash
# 错误示例（目标不能含 `*`）
hp ts hdev:~/logs/*.log ./out/*.log
```
