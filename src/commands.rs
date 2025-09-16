use anyhow::Result;

use crate::config::Config;
use crate::server::Server;
use crate::server::ServerCollection;

pub fn handle_create(config: &Config, alias: String, remote_host: String) -> Result<()> {
    let (username, address, port) = match crate::parse::parse_remote_host(&remote_host) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "new 命令参数错误: {}\n格式示例: hp new <alias> user@host[:port]",
                e
            );
            return Ok(());
        }
    };

    let mut collection = ServerCollection::read_from_storage(&config.server_file_path);
    if collection.get(&alias).is_some() {
        eprintln!("⚠️ 别名 '{}' 已存在", alias);
        return Ok(());
    }
    let server = Server {
        id: None,
        alias: Some(alias.clone()),
        username,
        address,
        port,
        last_connect: None,
    };
    collection.insert(&alias, server);
    collection.save_to_storage(&config.server_file_path);
    println!(
        "✅ 已创建别名 '{}' 并保存到 {}",
        alias,
        config.server_file_path.display()
    );
    Ok(())
}

pub fn handle_rename(config: &Config, alias: String, new_alias: String) -> Result<()> {
    let mut collection = ServerCollection::read_from_storage(&config.server_file_path);
    if collection.get(&alias).is_none() {
        eprintln!("❌ 别名 '{}' 不存在", alias);
        return Ok(());
    }
    if collection.get(&new_alias).is_some() {
        eprintln!("新别名 '{}' 已存在", new_alias);
        return Ok(());
    }

    if let Some(old) = collection.hosts().get(&alias).cloned() {
        collection.remove(&alias);
        let mut new_server = old.clone();
        new_server.alias = Some(new_alias.clone());
        collection.insert(&new_alias, new_server);
        collection.save_to_storage(&config.server_file_path);
        println!("已将别名 '{}' 重命名为 '{}'", alias, new_alias);
    }
    Ok(())
}

pub fn handle_list(config: &Config) -> Result<()> {
    let collection = ServerCollection::read_from_storage(&config.server_file_path);
    collection.show_table();
    Ok(())
}

pub fn handle_remove(config: &Config, alias: String) -> Result<()> {
    let mut collection = ServerCollection::read_from_storage(&config.server_file_path);
    if collection.get(&alias).is_none() {
        eprintln!("别名 '{}' 不存在", alias);
        return Ok(());
    }
    collection.remove(alias.as_str());
    collection.save_to_storage(&config.server_file_path);
    println!("✅ 已删除别名 '{}'", alias);
    Ok(())
}

pub fn handle_link(config: &Config, alias: String) -> Result<()> {
    let collection = ServerCollection::read_from_storage(&config.server_file_path);
    let Some(server) = collection.get(&alias as &str) else {
        eprintln!("❌ 别名 '{}' 不存在", alias);
        return Ok(());
    };

    // 检查本地公钥
    let pub_key = &config.pub_key_path;
    if !pub_key.exists() {
        eprintln!(
            "🔑 本地公钥不存在: {}\n请先使用 ssh-keygen 生成公钥，或在 config.json 配置 pub_key_path",
            pub_key.display()
        );
        return Ok(());
    }

    // 读取并验证本地公钥 — Read and validate local public key
    let key_content = match std::fs::read_to_string(pub_key) {
        Ok(s) => s.replace('\r', ""),
        Err(e) => {
            eprintln!("❌ 读取本地公钥失败: {}", e);
            return Ok(());
        }
    };
    let key_line = key_content.lines().next().unwrap_or("").trim().to_string();
    if key_line.is_empty() {
        eprintln!("⚠️ 公钥文件为空: {}", pub_key.display());
        return Ok(());
    }

    use std::process::{Command, Stdio};
    // 如果可用则优先使用 ssh-copy-id（在远端处理更好） — Prefer ssh-copy-id if available (better handling on remote)
    if which::which("ssh-copy-id").is_ok() {
        let status = Command::new("ssh-copy-id")
            .args([
                "-i",
                &config.pub_key_path.to_string_lossy(),
                &format!("-p{}", server.port),
                &format!("{}@{}", server.username, server.address),
            ])
            .status();
        match status {
            Ok(s) if s.success() => {
                println!(
                    "已使用 ssh-copy-id 安装公钥到 {}@{}",
                    server.username, server.address
                );
                return Ok(());
            }
            Ok(s) => eprintln!("ssh-copy-id 执行失败，退出码: {}", s.code().unwrap_or(-1)),
            Err(e) => eprintln!("无法执行 ssh-copy-id: {}", e),
        }
    }

    // 回退方案：通过 ssh stdin 发送公钥并执行远端安全追加脚本 — Fallback: send key via ssh stdin and run remote safe append script
    let remote_script = r#"
mkdir -p ~/.ssh && chmod 700 ~/.ssh
touch ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys
IFS= read -r KEY
# Use awk to compare key type + keydata (ignore comment)
k_type=$(printf '%s' "$KEY" | awk '{print $1}')
k_data=$(printf '%s' "$KEY" | awk '{print $2}')
if awk -v t="$k_type" -v d="$k_data" 'BEGIN{found=0} $0 !~ /^#/ {split($0,a," "); if(a[1]==t && a[2]==d){found=1; exit}} END{exit(found?0:1)}' ~/.ssh/authorized_keys; then
  echo "already-present"
  exit 0
fi
printf '%s\n' "$KEY" >> ~/.ssh/authorized_keys
echo "added"
"#;

    let mut child = match Command::new(&config.ssh_client_app_path)
        .args([
            format!("{}@{}", server.username, server.address),
            format!("-p{}", server.port),
            "bash -s".into(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "⚠️ 无法执行 ssh: {} (路径: {})",
                e,
                config.ssh_client_app_path.display()
            );
            return Ok(());
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write as _;
        // 发送公钥并随后发送远端脚本 — Send key followed by the remote script
        if let Err(e) = stdin.write_all(format!("{}\n", key_line).as_bytes()) {
            eprintln!("写入公钥到 ssh stdin 失败: {}", e);
        }
        if let Err(e) = stdin.write_all(remote_script.as_bytes()) {
            eprintln!("写入远端脚本失败: {}", e);
        }
    }

    match child.wait_with_output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if output.status.success() {
                if stdout.contains("already-present") {
                    println!("公钥已存在于 {}@{}", server.username, server.address);
                } else if stdout.contains("added") {
                    println!(
                        "已将本地公钥添加到 {}@{} 的 authorized_keys",
                        server.username, server.address
                    );
                } else {
                    println!("已完成公钥安装（远端输出: {}）", stdout.trim());
                }
            } else {
                eprintln!(
                    "❌ 远端处理失败，退出码: {}，输出: {}",
                    output.status.code().unwrap_or(-1),
                    stdout.trim()
                );
            }
        }
        Err(e) => eprintln!("❌ 等待 ssh 进程失败: {}", e),
    }

    Ok(())
}

// 基于 scp 的传输命令已移除；请使用外部 scp 或内置的 `ts` SFTP 实现 — scp-based transfer command has been removed; use external scp or the built-in `ts` SFTP in `transfer.rs`.

// 基于 SFTP 的传输（原 stsf -> 现 ts）实现已移动到 `src/transfer.rs` 并导出为 `handle_ts` — SFTP-based transfer (stsf -> ts) implementation moved to `src/transfer.rs` as `handle_ts`.

pub fn handle_set(
    config: &Config,
    pub_key_path: Option<std::path::PathBuf>,
    server_path: Option<std::path::PathBuf>,
    client_path: Option<std::path::PathBuf>,
    scp_path: Option<std::path::PathBuf>,
) -> Result<()> {
    let mut cfg = config.clone();
    if let Some(k) = pub_key_path {
        cfg.pub_key_path = k;
    }
    if let Some(s) = server_path {
        cfg.server_file_path = s;
    }
    if let Some(c) = client_path {
        cfg.ssh_client_app_path = c;
    }
    if let Some(scp) = scp_path {
        cfg.scp_app_path = scp;
    }
    // 写回配置文件（使用默认位置） — Write back to config file (use default location)
    cfg.save_to_storage();
    println!("✅ 配置已更新");
    Ok(())
}
