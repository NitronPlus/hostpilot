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

pub fn connect_session(server: &crate::server::Server) -> anyhow::Result<ssh2::Session> {
    let addr = format!("{}:{}", server.address, server.port);
    let mut addrs = addr.to_socket_addrs()?;
    let sock = addrs.next().ok_or_else(|| -> anyhow::Error {
        crate::TransferError::SshNoAddress(addr.clone()).into()
    })?;
    let tcp = TcpStream::connect_timeout(&sock, Duration::from_secs(10))?;
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = tcp.set_write_timeout(Some(Duration::from_secs(30)));
    let mut sess = ssh2::Session::new().map_err(|_| -> anyhow::Error {
        crate::TransferError::SshSessionCreateFailed(addr.clone()).into()
    })?;
    sess.set_tcp_stream(tcp);
    sess.handshake().map_err(|_| -> anyhow::Error {
        crate::TransferError::SshHandshakeFailed(addr.clone()).into()
    })?;
    if !sess.authenticated()
        && let Some(home_p) = dirs::home_dir()
    {
        for name in ["id_ed25519", "id_rsa", "id_ecdsa"] {
            let p = home_p.join(".ssh").join(name);
            if p.exists() {
                let _ = sess.userauth_pubkey_file(&server.username, None, &p, None);
                if sess.authenticated() {
                    break;
                }
            }
        }
    }
    if sess.authenticated() {
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
    if let Ok(mut addrs) = format!("{}:{}", server.address, server.port).to_socket_addrs()
        && let Some(sock) = addrs.next()
        && let Ok(tcp) = TcpStream::connect_timeout(&sock, Duration::from_secs(10))
    {
        let _ = tcp.set_read_timeout(Some(Duration::from_secs(30)));
        let _ = tcp.set_write_timeout(Some(Duration::from_secs(30)));
        if let Ok(mut sess) = ssh2::Session::new().map(|mut s| {
            s.set_tcp_stream(tcp);
            s
        }) {
            if sess.handshake().is_err() {
                return Err(crate::TransferError::SshHandshakeFailed(addr.to_string()).into());
            }
            if !sess.authenticated()
                && let Some(home_p) = dirs::home_dir()
            {
                for name in ["id_ed25519", "id_rsa", "id_ecdsa"] {
                    let p = home_p.join(".ssh").join(name);
                    if p.exists() {
                        let _ = sess.userauth_pubkey_file(&server.username, None, &p, None);
                        if sess.authenticated() {
                            break;
                        }
                    }
                }
            }
            if sess.authenticated() {
                *maybe_sess = Some(sess);
                return Ok(());
            }
        }
    }
    Err(crate::TransferError::WorkerBuildSessionFailed(format!(
        "{}:{}",
        server.address, server.port
    ))
    .into())
}
