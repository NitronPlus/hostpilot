use std::path::Path;

/// Trait abstracting SFTP operations used by workers. Return boxed readers/writers
/// so tests can inject mock file-like objects. Implementors must be Send so they
/// can be stored in worker threads as trait objects.
pub trait SftpLike: Send {
    fn stat_is_file(&self, p: &Path) -> Result<bool, String>;
    fn mkdir(&self, p: &Path, mode: i32) -> Result<(), String>;
    fn open_read(&self, p: &Path) -> Result<Box<dyn std::io::Read + Send>, String>;
    fn create_write(&self, p: &Path) -> Result<Box<dyn std::io::Write + Send>, String>;
}

/// Adapter that owns an `ssh2::Sftp` and implements `SftpLike` so it can be
/// boxed into a trait object for worker runtime use.
pub struct Ssh2Adapter(pub ssh2::Sftp);

impl SftpLike for Ssh2Adapter {
    fn stat_is_file(&self, p: &Path) -> Result<bool, String> {
        match self.0.stat(p) {
            Ok(st) => Ok(st.is_file()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn mkdir(&self, p: &Path, mode: i32) -> Result<(), String> {
        self.0.mkdir(p, mode).map_err(|e| e.to_string())
    }

    fn open_read(&self, p: &Path) -> Result<Box<dyn std::io::Read + Send>, String> {
        match self.0.open(p) {
            Ok(f) => Ok(Box::new(f)),
            Err(e) => Err(e.to_string()),
        }
    }

    fn create_write(&self, p: &Path) -> Result<Box<dyn std::io::Write + Send>, String> {
        match self.0.create(p) {
            Ok(f) => Ok(Box::new(f)),
            Err(e) => Err(e.to_string()),
        }
    }
}

impl Ssh2Adapter {
    pub fn into_inner(self) -> ssh2::Sftp {
        self.0
    }
}
