// PathBuf is not directly needed at top-level; individual enums use full paths.
/// Repository-wide structured errors for transfer-related operations.
#[derive(Debug, Clone)]
pub enum MkdirError {
    /// 目标存在且为文件（期望为目录）
    ExistsAsFile(std::path::PathBuf),
    /// SFTP 层返回的其他错误，保留路径与原始错误消息
    SftpError(std::path::PathBuf, String),
}

impl std::fmt::Display for MkdirError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MkdirError::ExistsAsFile(p) => {
                write!(f, "远端已有同名文件（期望目录）: {}", display_path(p))
            }
            MkdirError::SftpError(p, msg) => {
                write!(f, "创建远端目录失败: {} — {}", display_path(p), msg)
            }
        }
    }
}

impl std::error::Error for MkdirError {}

fn display_path(p: &std::path::Path) -> String {
    let s = p.to_string_lossy().to_string();
    if s.contains('\\') { s.replace('\\', "/") } else { s }
}

/// Higher-level transfer command errors that are useful to represent
/// programmatically instead of ad-hoc formatted strings.
#[derive(Debug, Clone)]
pub enum TransferError {
    InvalidDirection,
    UnsupportedGlobUsage(String),
    AliasNotFound(String),
    RemoteTargetMustBeDir(String),
    RemoteTargetParentMissing(String),
    CreateRemoteDirFailed(String, String),
    LocalTargetMustBeDir(String),
    LocalTargetParentMissing(String),
    CreateLocalDirFailed(String, String),
    GlobNoMatches(String),
    WorkerNoSession(String),
    WorkerNoSftp(String),
    SftpCreateFailed(String),
    // SSH / connection related
    SshNoAddress(String),
    SshSessionCreateFailed(String),
    SshHandshakeFailed(String),
    SshAuthFailed(String),
    WorkerBuildSessionFailed(String),
    // command validation / generic
    MissingLocalSource(String),
    DownloadMultipleRemoteSources(String),
    OperationFailed(String),
    WorkerIo(String),
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use TransferError::*;
        match self {
            InvalidDirection => {
                write!(f, "命令必须且只有一端为远端（alias:/path），请检查源和目标")
            }
            UnsupportedGlobUsage(s) => write!(f, "不支持的通配符用法（仅允许最后一段）：{}", s),
            AliasNotFound(a) => write!(f, "别名 '{}' 不存在", a),
            RemoteTargetMustBeDir(b) => write!(f, "目标必须存在且为目录: {} (远端)", b),
            RemoteTargetParentMissing(p) => {
                write!(f, "目标父目录不存在: {} (远端)，不会自动创建多级目录", p)
            }
            CreateRemoteDirFailed(path, msg) => write!(f, "创建远端目录失败: {} — {}", path, msg),
            LocalTargetMustBeDir(p) => {
                write!(f, "目标必须存在且为目录: {} (本地)，建议先创建或移除尾部/", p)
            }
            LocalTargetParentMissing(p) => {
                write!(f, "目标父目录不存在: {} (本地)，不会自动创建多级目录，请手动创建父目录", p)
            }
            CreateLocalDirFailed(path, msg) => {
                write!(f, "创建目标目录失败: {} (本地) — {}", path, msg)
            }
            GlobNoMatches(pat) => {
                write!(f, "glob 无匹配项（远端），请确认模式与路径是否正确: {}", pat)
            }
            WorkerNoSession(alias) => write!(f, "工作线程无法建立会话: {}", alias),
            WorkerNoSftp(alias) => write!(f, "工作线程无法创建 SFTP 会话: {}", alias),
            SftpCreateFailed(msg) => write!(f, "SFTP 创建失败: {}", msg),
            SshNoAddress(addr) => write!(f, "无法解析地址: {}", addr),
            SshSessionCreateFailed(addr) => write!(f, "无法创建 SSH Session: {}", addr),
            SshHandshakeFailed(addr) => write!(f, "SSH 握手失败: {}", addr),
            SshAuthFailed(addr) => write!(f, "SSH 认证失败: {}", addr),
            WorkerBuildSessionFailed(addr) => write!(f, "工作线程构建会话失败: {}", addr),
            MissingLocalSource(s) => write!(f, "缺少本地源: {}", s),
            DownloadMultipleRemoteSources(s) => write!(f, "下载仅支持单个远端源: {}", s),
            OperationFailed(s) => write!(f, "操作失败: {}", s),
            WorkerIo(s) => write!(f, "传输/IO 错误: {}", s),
        }
    }
}

impl std::error::Error for TransferError {}

impl TransferError {
    /// Whether this error is considered retriable when it occurs before an
    /// actual data transfer starts (session/SFTP establishment, pre-checks,
    /// mkdir checks, etc.). Conservative defaults: network/handshake related
    /// failures are retriable; validation/authorization failures are not.
    pub fn is_retriable_pre_transfer(&self) -> bool {
        use TransferError::*;
        match self {
            // retriable: transient connection/session issues
            SshSessionCreateFailed(_)
            | SshHandshakeFailed(_)
            | WorkerBuildSessionFailed(_)
            | SftpCreateFailed(_)
            | WorkerNoSession(_)
            | WorkerNoSftp(_) => true,
            // non-retriable: auth/validation/usage errors
            SshAuthFailed(_)
            | AliasNotFound(_)
            | InvalidDirection
            | UnsupportedGlobUsage(_)
            | MissingLocalSource(_)
            | RemoteTargetParentMissing(_)
            | RemoteTargetMustBeDir(_)
            | LocalTargetParentMissing(_)
            | LocalTargetMustBeDir(_)
            | GlobNoMatches(_)
            | CreateLocalDirFailed(_, _)
            | CreateRemoteDirFailed(_, _) => false,
            // fallback: treat unknown/generic as non-retriable by default
            _ => false,
        }
    }

    /// Whether this error is considered retriable when it occurs during an
    /// active data transfer (read/write/rename/sync). IO/network errors
    /// happening during streaming are generally retriable; logical/validation
    /// failures are not.
    pub fn is_retriable_during_transfer(&self) -> bool {
        use TransferError::*;
        match self {
            // transient IO/network errors -> retriable
            WorkerIo(_) | SftpCreateFailed(_) | WorkerNoSftp(_) | WorkerNoSession(_) => true,
            // non-retriable: permission/validation style errors
            SshAuthFailed(_)
            | AliasNotFound(_)
            | InvalidDirection
            | UnsupportedGlobUsage(_)
            | MissingLocalSource(_)
            | RemoteTargetParentMissing(_)
            | RemoteTargetMustBeDir(_)
            | LocalTargetParentMissing(_)
            | LocalTargetMustBeDir(_)
            | GlobNoMatches(_)
            | CreateLocalDirFailed(_, _)
            | CreateRemoteDirFailed(_, _)
            | DownloadMultipleRemoteSources(_)
            | OperationFailed(_) => false,
            // conservative default
            _ => false,
        }
    }
}
