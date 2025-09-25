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

When `--output-failures <path>` is provided, HP appends failed transfer items to
`<path>.jsonl` in JSON Lines format (one JSON object per line), which is convenient
for automation with `jq`/Python/Node.

Example:

```powershell
hp ts ./folder remote_alias:~/dest/ -c 8 --output-failures .\logs\transfer_failures
```

At the end of the command, the terminal prints the final JSONL file path:

```
Failures written to: .\logs\transfer_failures.jsonl
```

With `--json`, the one-line JSON summary also includes a `failures_path` field:

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
