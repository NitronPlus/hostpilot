use std::io::Read;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

pub fn expand_remote_tilde(sess: &ssh2::Session, path: &str) -> anyhow::Result<String> {
    let mut channel = sess.channel_session()?;
    // Try to print the remote home directory; fall back to '~' if unavailable
    let _ = channel.exec("printf '%s' \"$HOME\" || echo '~'");
    let mut s = String::new();
    channel.read_to_string(&mut s).ok();
    channel.wait_close().ok();
    let home = s.lines().next().unwrap_or("~").trim().to_string();
    let tail = path.trim_start_matches('~').trim_start_matches('/');
    let expanded =
        if tail.is_empty() { home } else { format!("{}/{}", home.trim_end_matches('/'), tail) };
    Ok(expanded)
}

/// SSH 密钥认证的通用逻辑
fn try_key_authentication(sess: &mut ssh2::Session, username: &str) -> bool {
    if sess.authenticated() {
        return true;
    }
    if let Some(home_p) = dirs::home_dir() {
        for name in ["id_ed25519", "id_rsa", "id_ecdsa"] {
            let p = home_p.join(".ssh").join(name);
            if p.exists() {
                let _ = sess.userauth_pubkey_file(username, None, &p, None);
                if sess.authenticated() {
                    return true;
                }
            }
        }
    }
    false
}

/// 创建并配置 TCP 连接
fn create_tcp_connection(addr: &str) -> anyhow::Result<TcpStream> {
    let mut addrs = addr.to_socket_addrs()?;
    let sock = addrs.next().ok_or_else(|| -> anyhow::Error {
        crate::TransferError::SshNoAddress(addr.to_string()).into()
    })?;
    let tcp = TcpStream::connect_timeout(&sock, Duration::from_secs(10))?;
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = tcp.set_write_timeout(Some(Duration::from_secs(30)));
    Ok(tcp)
}

pub fn connect_session(server: &crate::server::Server) -> anyhow::Result<ssh2::Session> {
    let addr = format!("{}:{}", server.address, server.port);
    let tcp = create_tcp_connection(&addr)?;
    let mut sess = ssh2::Session::new().map_err(|_| -> anyhow::Error {
        crate::TransferError::SshSessionCreateFailed(addr.clone()).into()
    })?;
    sess.set_tcp_stream(tcp);
    sess.handshake().map_err(|_| -> anyhow::Error {
        crate::TransferError::SshHandshakeFailed(addr.clone()).into()
    })?;

    if try_key_authentication(&mut sess, &server.username) {
        Ok(sess)
    } else {
        Err(crate::TransferError::SshAuthFailed(addr.clone()).into())
    }
}

pub fn ensure_worker_session(
    maybe_sess: &mut Option<ssh2::Session>,
    server: &crate::server::Server,
    addr: &str,
) -> anyhow::Result<()> {
    if maybe_sess.is_some() {
        return Ok(());
    }

    let server_addr = format!("{}:{}", server.address, server.port);
    match create_tcp_connection(&server_addr) {
        Ok(tcp) => {
            if let Ok(mut sess) = ssh2::Session::new() {
                sess.set_tcp_stream(tcp);
                if sess.handshake().is_err() {
                    return Err(crate::TransferError::SshHandshakeFailed(addr.to_string()).into());
                }

                if try_key_authentication(&mut sess, &server.username) {
                    *maybe_sess = Some(sess);
                    Ok(())
                } else {
                    Err(crate::TransferError::WorkerBuildSessionFailed(server_addr).into())
                }
            } else {
                Err(crate::TransferError::WorkerBuildSessionFailed(server_addr).into())
            }
        }
        Err(_) => Err(crate::TransferError::WorkerBuildSessionFailed(server_addr).into()),
    }
}

/// 统一的会话+SFTP准备函数，减少upload/download重复逻辑
pub fn ensure_session_and_sftp(
    maybe_sess: &mut Option<ssh2::Session>,
    maybe_sftp: &mut Option<Box<dyn crate::transfer::sftp_like::SftpLike>>,
    server: &crate::server::Server,
    addr: &str,
    session_rebuilds: &mut u32,
    sftp_rebuilds: &mut u32,
) -> anyhow::Result<()> {
    // Ensure session first
    if maybe_sess.is_none() {
        ensure_worker_session(maybe_sess, server, addr)?;
        *session_rebuilds += 1;
    }

    let sess = maybe_sess.as_mut().ok_or_else(|| -> anyhow::Error {
        crate::TransferError::WorkerNoSession(
            server.alias.clone().unwrap_or_else(|| "<unknown>".to_string()),
        )
        .into()
    })?;

    // Ensure SFTP if not present
    if maybe_sftp.is_none() {
        match sess.sftp() {
            Ok(s) => {
                *sftp_rebuilds += 1;
                *maybe_sftp = Some(Box::new(crate::transfer::sftp_like::Ssh2Adapter(s)));
            }
            Err(e) => {
                return Err(crate::TransferError::SftpCreateFailed(format!("{}", e)).into());
            }
        }
    }

    Ok(())
}
