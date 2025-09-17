use anyhow::Result;

pub fn parse_remote_host(input: &str) -> Result<(String, String, u16)> {
    let at_pos =
        input.find('@').ok_or_else(|| anyhow::anyhow!("缺少用户名，例如 user@host[:port]"))?;
    let (user_part, host_part) = input.split_at(at_pos);
    let user = user_part.trim();
    let host_port = &host_part[1..]; // skip '@'
    if user.is_empty() || host_port.is_empty() {
        return Err(anyhow::anyhow!("用户名或主机为空"));
    }

    // 支持 host:port，否则默认 22 — Support host:port, default to 22 if not provided
    let (host, port) = if let Some(colon) = host_port.rfind(':') {
        let (h, p_str) = host_port.split_at(colon);
        let p_str = &p_str[1..]; // skip ':'
        let p: u16 = p_str.parse().map_err(|_| anyhow::anyhow!("端口无效: {}", p_str))?;
        (h.to_string(), p)
    } else {
        (host_port.to_string(), 22)
    };

    Ok((user.to_string(), host, port))
}

pub fn parse_alias_and_path(input: &str) -> Result<(String, String)> {
    if let Some((alias, rest)) = input.split_once(':') {
        let a = alias.trim();
        let p = rest.trim();
        if a.is_empty() || p.is_empty() {
            return Err(anyhow::anyhow!("别名或路径为空"));
        }
        Ok((a.to_string(), p.to_string()))
    } else {
        Err(anyhow::anyhow!("未找到分隔符 ':'"))
    }
}
