![License: MIT/Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-orange.svg)

<!-- Language switch -->
- [中文 / Chinese](./README_ZH.md)

# PSM — Personal SSH Server Manager

PSM is a cross-platform CLI tool to manage multiple remote SSH server aliases, quickly connect to servers, and transfer files using the built-in SFTP command (`ts`).

[Change Log](CHANGELOG.md)

---

## Quick Start

1. List saved server aliases:

```powershell
psm ls
```

2. Create a server alias:

```powershell
psm new mybox root@example.com:22
```

3. Connect using an alias:

```powershell
psm mybox
```

4. Transfer files with the built-in SFTP (`ts`):

Single file upload:

```powershell
psm ts ./localfile.txt remote_alias:~/dest/path/
```

Directory or multiple sources upload:

```powershell
psm ts ./folder/ ./another.txt remote_alias:~/dest/path/ -c 6
```

Concurrency option:

- `-c, --concurrency <N>`: Number of concurrent workers, default 6, maximum 8 (0 treated as 1).

Example (4 workers):

```powershell
psm ts ./largefile.bin remote_alias:~/backup/ -c 4
```

More `ts` usage details are documented in `TRANSFER.md`.

---

## Installation

Build from source:

```powershell
# Requires Rust toolchain (rustc + cargo)
cargo build --release
# The binary will be available at target/release/psm
```

Windows packaging and distribution depend on release artifacts; build from source if no official binaries are provided.

Requires `ssh` (client) available in PATH for interactive connections; `ts` uses the `ssh2` crate (libssh2) and does not support interactive password prompts.

---

## Commands & Examples

- `psm new <alias> user@host[:port]` — Create alias
- `psm ls` — List aliases
- `psm <alias>` — Open interactive SSH session using the system ssh client
- `psm ts <sources...> <target>` — Built-in SFTP transfer (sources may be local paths or remote alias:/path)
- `psm ln <alias>` — Install local public key to remote `authorized_keys`

Example: upload a local directory recursively:

```powershell
psm ts C:\data\project\ remote_alias:~/backup/project/ -c 6
```

Download a remote file to local:

```powershell
psm ts remote_alias:~/logs/sys.log C:\tmp\sys.log
```

See `TRANSFER.md` for more details.

---

## FAQ

Q: Does `ts` support interactive password prompts?

A: No. The built-in `ts` uses `ssh2` (libssh2) and does not support interactive password prompts. Use SSH public-key authentication or configure a usable private key in your environment.

Q: How do I set the default ssh client or public key path?

A: Use the `psm set` subcommand, for example:

```powershell
psm set -c "C:\Windows\System32\OpenSSH\ssh.exe" -k "C:\Users\you\.ssh\id_rsa.pub"
```

Q: Can I disable many per-file progress bars in non-verbose mode?

A: In non-verbose mode `ts` shows only an aggregate progress bar or limits the number of simultaneous file progress bars to reduce terminal noise.

---

## Contributing

Welcome to open issues or PRs. Please run `cargo fmt` and `cargo clippy` before contributing, and keep commits small and descriptive (Chinese commit messages are preferred).

Suggested workflow:

1. Fork the repo
2. Create a branch
3. Implement and test
4. Open a PR with a clear description (in Chinese)

---

## License

This project is dual-licensed under Apache-2.0 or MIT. See `LICENSE-APACHE` and `LICENSE-MIT` in the repo root.

---

If you want me to adjust the English tone or add more `ts` examples (glob patterns, directory semantics, failure-report examples), tell me and I will extend the documentation.
