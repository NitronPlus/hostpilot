![License: MIT/Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-orange.svg)

<!-- Language switch -->
- [中文 / Chinese](./README_ZH.md)

# HostPilot (HP) — Personal SSH Server Manager

HostPilot (HP) is a lightweight, cross-platform CLI tool designed to simplify daily
management of multiple remote SSH hosts.

- Manage SSH server aliases to avoid repeatedly typing long hostnames, ports, and
	usernames.
- Open interactive SSH sessions using the system `ssh` client to retain the native
	terminal experience.
- Perform high-performance, concurrent file transfers with the built-in SFTP
	subcommand `ts`, suitable for large or batch transfers with configurable
	concurrency.

HostPilot focuses on usability and automation: use the system `ssh` for interactive
logins and the built-in `ts` for scripted transfers. Because `ts` relies on
`ssh2` (libssh2), it does not support interactive password prompts — public-key
authentication is recommended for unattended workflows.

[Change Log](CHANGELOG.md)

---

## Quick Start

1. List saved server aliases:
```powershell
hp ls
```

2. Create a server alias:

```powershell
hp new mybox root@example.com:22
```

3. Connect using an alias:

```powershell
hp mybox
```

4. Transfer files with the built-in SFTP (`ts`):

Single file upload:

```powershell
hp ts ./localfile.txt remote_alias:~/dest/path/
```

Directory or multiple sources upload:

```powershell
hp ts ./folder/ ./another.txt remote_alias:~/dest/path/ -c 8
```

Concurrency option:

- `-c, --concurrency <N>`: Number of concurrent workers. Default is 8, maximum is
	16 (0 treated as 1).

Example (4 workers):

```powershell
hp ts ./largefile.bin remote_alias:~/backup/ -c 4
```

More `ts` usage details are documented in `TRANSFER.md`.

Documentation:

- Transfer details: see `TRANSFER.md` for full examples and the semantics of `ts`
	(upload, download, globs, concurrency, and failure handling).

### Failure output (JSONL)

Failures are always recorded as JSON Lines (JSONL) in HostPilot's canonical
logs directory: `~/.hostpilot/logs/`. The default file name is `failures.jsonl`.
The file is append-only; at the end of a run the program prints the path to the
file that was written so automation can consume it. This behavior is not
configurable via CLI.

Single failure JSON object example (one line in the JSONL file):

```
{"variant":"WorkerIo","message":"local open failed: C:\\path\\to\\file2"}
```

Fields and common variants:

- variant: Discriminant for the failure category. Common values:
	- InvalidDirection — CLI usage error: both sides local or both remote.
	- UnsupportedGlobUsage — Invalid wildcard usage; only the last path segment may contain `*`/`?`.
	- AliasNotFound — The given alias does not exist.
	- RemoteTargetMustBeDir / LocalTargetMustBeDir — Target must exist and be a directory.
	- RemoteTargetParentMissing / LocalTargetParentMissing — Parent directory missing.
	- CreateRemoteDirFailed / CreateLocalDirFailed — Failed to create (path + error).
	- GlobNoMatches — Glob pattern had no matches on the source side.
	- WorkerNoSession / WorkerNoSftp — Worker failed to establish session/SFTP.
	- SftpCreateFailed — Creating the SFTP handle failed.
	- SshNoAddress — Could not resolve address.
	- SshSessionCreateFailed / SshHandshakeFailed — Session creation or handshake failed.
	- SshAuthFailed — Authentication failed.
	- WorkerBuildSessionFailed — Worker failed to build session.
	- MissingLocalSource — Local source path missing.
	- DownloadMultipleRemoteSources — Download supports only a single remote source.
	- OperationFailed — Generic operation failure.
	- WorkerIo — IO/transfer error (message contains details).

- message: Human-readable message; safe for logs.
- alias / addr: When present, the alias or resolved address that failed.
- path / pattern: Path involved (e.g., target path) or the glob pattern.
- error / detail: Additional string detail (nested error or auxiliary info).

---

Quiet mode and CI-friendly JSON summary

For automation/CI runs prefer `--quiet` together with `--json`. In quiet
mode HostPilot suppresses human-friendly progress and summary lines. When
`--json` is also given the program emits a single-line JSON summary at the end
that is suitable for programmatic consumption. The summary includes fields
like `total_bytes`, `elapsed_secs`, `files`, and `failures` (count). If any
failures were written the summary also contains `failures_path` with the
canonical `failures.jsonl` path.

Example (recommended for CI):

PowerShell (capture summary and inspect failures):

```powershell
# Run transfer in quiet+json mode and capture the single-line JSON summary
$summary = hp ts ./localdir remote_alias:~/dest --quiet --json
if ($summary) {
	$obj = $summary | ConvertFrom-Json
	Write-Output "Files: $($obj.files)  Bytes: $($obj.total_bytes)  Failures: $($obj.failures)"
	if ($obj.failures -gt 0 -and $obj.failures_path) {
		# failures.jsonl is JSON Lines; parse each line
		Get-Content $obj.failures_path | ForEach-Object { $_ | ConvertFrom-Json } | Out-File failures_parsed.json
		Write-Output "Parsed failures written to failures_parsed.json"
	}
}
```

Bash / sh (capture summary and inspect failures with jq):

```sh
# Run transfer in quiet+json mode and capture the single-line JSON summary
summary=$(hp ts ./localdir remote_alias:~/dest --quiet --json)
echo "$summary" | jq '.'
fail_path=$(echo "$summary" | jq -r '.failures_path')
if [ -n "$fail_path" ] && [ -f "$fail_path" ]; then
  # Convert JSONL to a JSON array for easier consumption
  jq -s '.' "$fail_path" > failures.json
  echo "Parsed failures written to: failures.json"
fi
```

The examples above show a typical CI pattern: run the command in quiet+json,
parse the one-line JSON summary for quick pass/fail decisions, and if failures
are present read `failures.jsonl` to provide a structured failure report.

Edge cases & failure-write fallbacks

- What "quiet" hides (and what it doesn't): `--quiet` suppresses the human-
	oriented progress bars and summary lines that are printed to stdout. It does
	not suppress machine-facing outputs: when `--json` is used the single-line
	JSON summary is still printed to stdout. For operational issues (for
	example, inability to write the `failures.jsonl` file) HostPilot will emit a
	concise one-line warning to stderr so CI/debugging tooling can surface the
	condition. In short: quiet = no human progress on stdout, but critical
	warnings are still printed to stderr and the JSON summary (when requested)
	remains on stdout.

- Failures file write semantics and what to expect in CI:
	- HostPilot attempts to write failures to the canonical logs directory
		(typically `~/.hostpilot/logs/failures.jsonl`). If the write succeeds the
		JSON summary will include `failures_path` pointing at that file.
	- If HostPilot cannot create or write to the canonical logs directory (for
		example, permission denied), it will attempt to create the directory. If
		creation or the subsequent write still fails, HostPilot will print a
		single-line warning to stderr describing the failure. In that case the
		JSON summary may still indicate a non-zero `failures` count but will not
		contain `failures_path`.
	- The program does not change the exit code when a failures write fails; the
		stderr warning exists solely to make the condition discoverable by CI
		systems. (If you require different semantics, capture stderr in your CI
		job and fail the job on the presence of the warning.)

- How to capture both JSON summary and any stderr warnings in CI
	- PowerShell (capture stdout JSON and redirect stderr to a file):

```powershell
# Run and capture JSON summary on stdout, and any warnings on stderr
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
		# If stdout was empty, the JSON summary may have been emitted to stderr due to environment; inspect the error file.
		if (Test-Path hp_ts.err) { Get-Content hp_ts.err | Write-Output }
}
```

	- Bash / sh (capture stdout JSON and stderr separately):

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

Notes:

- The `failures.jsonl` file is append-only and may contain entries from
	previous runs. In CI you typically convert the JSONL to an array (for
	example `jq -s '.' failures.jsonl`) before uploading as an artifact.
- If you need deterministic, per-run failures files, collect the canonical
	`failures.jsonl` and filter by timestamp or rotate it in your CI job. HostPilot
	does not currently rotate the failures file automatically.

### In scripts / CI: locating failures file

Automation can also directly read the fixed, append-only `failures.jsonl` file from the
canonical logs directory. Example snippets:

- PowerShell (Windows/CI runner):

```powershell
$failPath = Join-Path $env:USERPROFILE (".hostpilot\\logs\\failures.jsonl")
if (Test-Path $failPath) {
	Get-Content $failPath | Select-String -Pattern '"variant"' | Out-File -FilePath "./failures_summary.txt"
	Write-Output "Failures written to: $failPath"
} else {
	Write-Output "No failures file found at $failPath"
}
```

- Bash / sh (Unix-like CI):

```sh
fail_path="$HOME/.hostpilot/logs/failures.jsonl"
if [ -f "$fail_path" ]; then
	grep '"variant"' "$fail_path" > failures_summary.txt
	echo "Failures written to: $fail_path"
else
	echo "No failures file found at $fail_path"
fi
```

## Installation

Build from source:

```powershell
# Requires Rust toolchain (rustc + cargo)
cargo build --release
# The binary will be available at target/release/hp
```

Windows packaging and distribution depend on release artifacts; build from source
if no official binaries are provided.

Requires `ssh` (client) available in PATH for interactive connections; `ts` uses
the `ssh2` crate (libssh2) and does not support interactive password prompts.

---

## Commands & Examples

- `hp new <alias> user@host[:port]` — Create alias
- `hp ls` — List aliases
- `hp <alias>` — Open interactive SSH session using the system `ssh` client
- `hp ts <sources...> <target>` — Built-in SFTP transfer (sources may be local
	paths or remote `alias:/path`)
- `hp ln <alias>` — Install local public key to remote `authorized_keys`

Example: upload a local directory recursively:

```powershell
hp ts C:\data\project\ remote_alias:~/backup/project/ -c 6
```

Download a remote file to local:

```powershell
hp ts remote_alias:~/logs/sys.log C:\tmp\sys.log
```

See `TRANSFER.md` for more details.

---

## FAQ

Q: Does `ts` support interactive password prompts?

A: No. The built-in `ts` uses `ssh2` (libssh2) and does not support interactive
password prompts. Use SSH public-key authentication or configure a usable
private key in your environment.

Q: How do I set the default ssh client or public key path?

A: Use the `hp set` subcommand, for example:

```powershell
hp set -c "C:\Windows\System32\OpenSSH\ssh.exe" -k "C:\Users\you\.ssh\id_rsa.pub"
```

Q: Can I disable many per-file progress bars in non-verbose mode?

A: In non-verbose mode `ts` shows only an aggregate progress bar or limits the
number of simultaneous file progress bars (visible cap: 8) to reduce terminal noise.

---

## Contributing

Welcome to open issues or PRs. Please run `cargo fmt` and `cargo clippy` before
contributing, and keep commits small and descriptive (Chinese commit messages are
preferred).

Suggested workflow:

1. Fork the repo
2. Create a branch
3. Implement and test
4. Open a PR with a clear description (in Chinese)

---

## License

This project is dual-licensed under Apache-2.0 or MIT. See `LICENSE-APACHE` and
`LICENSE-MIT` in the repo root.
