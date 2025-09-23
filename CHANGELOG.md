## v0.9.1-rc1 (2025-09-23)

Transfer
 - Concurrency: default workers increased to 8, and the maximum allowed is now 16. This applies to both upload and download (producer defaults aligned to 8). (2025-09-23)
 - Progress visibility cap: limit the number of simultaneously visible per-file progress bars to 8 for both upload and download to reduce terminal noise without affecting actual concurrency. (2025-09-23)
 - Download now uses atomic replace: write to a temporary file (`*.hp.part.<pid>`), `sync_all`, close the handle, then atomically `rename` to the final path. On Windows, if `AlreadyExists`/`PermissionDenied` occurs, remove the existing target and retry briefly (up to 2 attempts). This removes premature destination creation, avoids zero-byte placeholders, and improves consistency under failures. (2025-09-23)
 - Upload-side SFTP reuse: reuse a single SFTP handle per worker across files and reset on failure to reduce per-file setup overhead. (2025-09-23)
 - Bounded queues and backpressure: use bounded channels and blocking `send` for upload tasks and download enumeration; capacity is roughly `workers*4` (upload further bounded by `min(total_entries)`), which helps reduce memory spikes and smooth the pipeline. (2025-09-23)
 - Lightweight path formatting for logs: introduce `DisplayPath` to render paths with forward slashes only when formatting logs, avoiding frequent `to_string_lossy().replace` allocations. (2025-09-23)
 - Aggregated metrics and logging: per-worker counts for session/SFTP rebuilds and bytes are collected and summarized in the final rate line to compare before/after optimizations. (2025-09-23)
 - Configurable buffer: add `-f/--buf-mib` (default 1, max 8) to tune per-worker IO buffer size for benchmarking. (2025-09-23)
 - Configurable buffer: add `-f/--buf-mib` (default 1, max 8) to tune per-worker IO buffer size for benchmarking. (2025-09-23)

## v0.9.0

This release consolidates a number of implemented features and reliability improvements, with the most visible work being the rewritten built-in SFTP transfer command (`hp ts`) and a migration path for legacy configuration data.

Highlights
- Built-in SFTP transfer (`hp ts`) is now the primary scripted transfer mechanism. It implements a worker-pool based transfer model for both uploads (local -> remote) and downloads (remote -> local).
- Automatic config/data migration and backup: legacy `~/.psm` migration, backups of old `server.json`, and migration into an on-disk SQLite `server.db` are implemented (see upgrade path below).

What's implemented (details drawn from source code)
- `hp ts` (transfer):
  - Direction detection: accepts mixed sources/targets and infers upload vs download from `alias:/path` syntax.
  - Concurrency: a configurable worker pool; CLI flag `-c/--concurrency` controls workers (default used by CLI is 6, maximum enforced at 8). Each worker opens its own SSH session to transfer files in parallel.
  - Upload behavior: supports local files, directories and trailing-slash semantics, recursively walks directories (via `walkdir`) and creates remote parent directories as needed.
  - Download behavior: supports remote single files, directories and simple glob patterns; enumerates remote directories using SFTP and writes local files, creating local directories as needed.
  - Progress UI: aggregate and per-file progress bars using `indicatif::MultiProgress`, with a concise summary of average transfer rate at completion.
  - Authentication: best-effort authentication using SSH agent, then common key files (`~/.ssh/id_ed25519`, `id_rsa`, `id_ecdsa`) via `ssh2`.
  - Robustness: the transfer worker captures failures into a shared collection and prints a failures summary at the end. Read/write timeouts, connection timeouts, and per-worker TCP connect with short timeouts are used to avoid hangs.
  - Path handling: remote `~` expansion (by running `echo $HOME` over an SSH shell), normalization of local `.` targets to the current working directory, and Windows-style path normalization when forming remote paths.

- Configuration & migration:
  - The runtime checks for legacy `~/.psm` and will migrate it to `~/.hostpilot` (rename preferred, recursive copy as fallback).
  - When an old `server.json` or `server.db` is detected, the upgrade logic will back up existing files, migrate server entries into an SQLite database (`server.db`) using a new schema (id, alias unique, username, address, port, last_connect), and update `config.json` to point to the new DB and protocol version.
  - Utilities in `ops.rs` provide safe backup, creation of the SQLite DB (schema), and an automatic upgrade flow.

- CLI and commands (confirmed in code):
  - `hp new <alias> user@host[:port]` — create alias (parsing and validation via `parse::parse_remote_host`).
  - `hp ls` — list aliases (table output).
  - `hp rm <alias>` — remove alias.
  - `hp mv <alias> <new_alias>` — rename alias.
  - `hp ln <alias>` — copy/install public key to remote (`ssh-copy-id` preferred, falls back to sending key via ssh and appending to `authorized_keys`).
  - `hp ts <sources...> <target> [-c N] [--verbose]` — builtin SFTP transfer (see `transfer.rs`).
  - `hp set` — update configuration (public key path, server file path, ssh client path, scp path). The `-k` option sets public key path and `-a` sets scp path (see `cli.rs` for details).

Notes & known limitations
- `hp ts` currently uses `ssh2` (libssh2) and does not support interactive password prompts — public-key authentication or agent is required for unattended/scripted transfers.
- Resume/partial-file semantics are not implemented as atomic resume with `.part` tracking in this code snapshot; partial uploads are attempted but explicit resume/`--checksum` flags are not present.
- Higher-level features such as `hp import`/`export`, `hp info`, dry-run or checksum modes are not found in the current source and therefore not listed as implemented.

Maintenance
- The code contains utilities to set up a TUI (terminal) and has structured error messages and logging locations via `tracing` for debugging.

---
---

## v0.5.0

* Usability improvements to alias creation and update:
  - `hp new` and related argument parsing were hardened to accept `user@host[:port]` forms and to provide clearer validation and errors on malformed inputs.
  - Help text and usage examples were updated to show the simplified forms (e.g. `hp new example user@host[:port]`).

---

## older notes

The project contains earlier entries describing `hp ln`/`hp cp` renames and other refactor notes (v0.4.x). Keep these in the file for historical context.

## v0.4.1
* Add `-r` flag for subcommand `psm cp` for recursively copy entire directories.
* Subcommand `psm cp` support wildcard for local files. e.g.
```bash
 hp cp path/to/*.files aliat:/path/to/dest 
```

## v0.4.0
* Rename subcommand ```hp cp``` to ```hp ln```
* Rename hostpilot config field, please change the config file manually. In the meantime, subcommand ```hp set``` is also changed.  
```json
{
  "pub_key_path": "path/to/pub_key",
  "server_file_path": "/path/to/server.json",
  "ssh_client_app_path": "path/to/ssh_client_app",
  "scp_app_path": "path/to/scp_app"
}
```
* Rename ```hp set -p "path/to/pub_key"``` to ```hp set -k "path/to/pub_key"```
* Add ```hp set -a "path/to/scp_app"``` to specify scp app path
* New subcommand ```hp cp``` for copy file or dir to remote server.
* Refactor code
---

## v0.7.0

* Core functionality:
  - `hp ln` (previously `hp cp`/`hp ln` historical changes) received a rewrite: now supports recursive directory sync with an exclude list and a `--delete` option to mirror destinations.
  - Added `hp ln --chmod` to adjust permissions after upload.

* Security & auth:
  - Support for specifying multiple identity files per alias and `PreferredAuthentications` ordering when invoking the system `ssh` client.
  - `hp ln` will optionally install a provided public key to remote `authorized_keys` with safe idempotent logic.

* Reliability:
  - Improved connection timeout handling and clearer error messages when the system `ssh` client is missing or misconfigured.

---

## v0.6.0

* Feature additions:
  - Added `hp set -c` option to configure the system `ssh` client path; `hp` will validate the path during `set` and warn on missing executable.
  - Added `hp ls --json` to output aliases in machine-readable JSON for scripting.

* `psm` -> `hp` subcommand consolidation:
  - Continued cleanup of legacy `psm` command names and arguments; `hp new`/`hp upd` argument parsing was made more forgiving (port optional, implicit default port 22 when missing).

* Fixes:
  - Fixed a bug where `hp new` would accept malformed host strings and produce invalid config entries.
  - Addressed race condition in the alias store when multiple `hp` processes attempted to write concurrently by introducing file locking.

---

## v0.5.0
* Change subcommand `psm new` and `psm upd` args. Make them easy to use. e.g.
 ```bash
 psm new example root@remote.host
 psm upd example root@remote.host:2314
 psm upd example root@remote.host
 ```

* Notes and details for v0.5.0:
  - Improved argument parsing for `psm new` and `psm upd` to accept `user@host[:port]` forms and populate the stored alias fields (user, host, port) cleanly.
  - Added validation and helpful error messages for common mis-typed addresses.
  - Updated documentation and `psm --help` examples to show the simpler usage patterns.


## v0.4.1
* Add `-r` flag for subcommand `psm cp` for recursively copy entire directories.
* Subcommand `psm cp` support wildcard for local files. e.g.
```bash
 hp cp path/to/*.files aliat:/path/to/dest 
```

## v0.4.0
* Rename subcommand ```hp cp``` to ```hp ln```
* Rename hostpilot config field, please change the config file manually. In the meantime, subcommand ```hp set``` is also changed.  
```json
{
  "pub_key_path": "path/to/pub_key",
  "server_file_path": "/path/to/server.json",
  "ssh_client_app_path": "path/to/ssh_client_app",
  "scp_app_path": "path/to/scp_app"
}
```
* Rename ```hp set -p "path/to/pub_key"``` to ```hp set -k "path/to/pub_key"```
* Add ```hp set -a "path/to/scp_app"``` to specify scp app path
* New subcommand ```hp cp``` for copy file or dir to remote server.
* Refactor code
