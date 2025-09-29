use std::collections::VecDeque;

use crate::transfer::sftp_like::Ssh2Adapter;

/// Error returned when attempting to acquire an SFTP channel from the pool.
#[derive(Debug)]
pub enum AcquireError {
    /// All configured SFTP channels are currently checked out.
    NoCapacity,
    /// Creating a fresh SFTP channel from the underlying session failed.
    Create(ssh2::Error),
}

impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcquireError::NoCapacity => write!(f, "no free SFTP channel"),
            AcquireError::Create(err) => write!(f, "failed to create SFTP channel: {err}"),
        }
    }
}

impl std::error::Error for AcquireError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AcquireError::Create(err) => Some(err),
            AcquireError::NoCapacity => None,
        }
    }
}

/// Simple SFTP channel pool backed by a single SSH session. Channels are lazily
/// constructed and kept alive until reset. Borrowing a channel returns a guard
/// that will automatically return the channel to the pool when dropped.
pub struct MultiChannelSftpManager {
    channels: Vec<Option<ssh2::Sftp>>,
    free: VecDeque<usize>,
    session: ssh2::Session,
}

impl MultiChannelSftpManager {
    /// Create a new manager with up to `max_channels` SFTP handles. A value of 0
    /// falls back to 1 to ensure at least one channel is available.
    pub fn new(session: ssh2::Session, max_channels: usize) -> Self {
        let capacity = max_channels.max(1);
        let mut free = VecDeque::with_capacity(capacity);
        for idx in 0..capacity {
            free.push_back(idx);
        }
        let mut channels = Vec::with_capacity(capacity);
        channels.resize_with(capacity, || None);
        Self { channels, free, session }
    }

    /// Total number of channels managed by this pool.
    #[allow(dead_code)]
    pub fn capacity(&self) -> usize {
        self.channels.len()
    }

    /// Number of channels currently available for checkout.
    #[allow(dead_code)]
    pub fn available(&self) -> usize {
        self.free.len()
    }

    /// Drop all cached channels but keep the underlying session alive. Future
    /// acquire calls will recreate channels lazily.
    pub fn reset(&mut self) {
        for slot in &mut self.channels {
            *slot = None;
        }
        self.free.clear();
        for idx in 0..self.channels.len() {
            self.free.push_back(idx);
        }
    }

    /// Attempt to borrow an SFTP channel. Returns an RAII guard that exposes the
    /// SFTP adapter and returns it to the pool automatically when dropped.
    pub fn acquire(&mut self) -> Result<SftpChannelGuard<'_>, AcquireError> {
        let idx = self.free.pop_front().ok_or(AcquireError::NoCapacity)?;
        if let Some(existing) = self.channels[idx].take() {
            Ok(SftpChannelGuard::new(self, idx, existing, false))
        } else {
            match self.session.sftp() {
                Ok(sftp) => Ok(SftpChannelGuard::new(self, idx, sftp, true)),
                Err(err) => {
                    self.free.push_front(idx);
                    Err(AcquireError::Create(err))
                }
            }
        }
    }

    fn release_slot(&mut self, idx: usize, sftp: ssh2::Sftp) {
        debug_assert!(idx < self.channels.len());
        debug_assert!(self.channels[idx].is_none());
        self.channels[idx] = Some(sftp);
        self.free.push_back(idx);
    }
}

/// Guard returned by `MultiChannelSftpManager::acquire`. When the guard is
/// dropped, the SFTP channel is automatically returned to the pool.
pub struct SftpChannelGuard<'a> {
    manager: &'a mut MultiChannelSftpManager,
    idx: usize,
    adapter: Option<Ssh2Adapter>,
    fresh: bool,
}

impl<'a> SftpChannelGuard<'a> {
    fn new(
        manager: &'a mut MultiChannelSftpManager,
        idx: usize,
        sftp: ssh2::Sftp,
        fresh: bool,
    ) -> Self {
        Self { manager, idx, adapter: Some(Ssh2Adapter(sftp)), fresh }
    }

    /// Access the underlying adapter as an immutable reference.
    pub fn adapter(&self) -> &Ssh2Adapter {
        self.adapter.as_ref().expect("adapter should be present")
    }

    /// Whether the guard had to construct a fresh SFTP channel when acquired.
    pub fn was_fresh(&self) -> bool {
        self.fresh
    }
}

impl Drop for SftpChannelGuard<'_> {
    fn drop(&mut self) {
        if let Some(adapter) = self.adapter.take() {
            self.manager.release_slot(self.idx, adapter.into_inner());
        } else {
            // In practice the adapter should always be Some, but ensure slot is
            // still marked available if it was manually taken.
            self.manager.free.push_back(self.idx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_falls_back_to_one() {
        let session = ssh2::Session::new().expect("session");
        let manager = MultiChannelSftpManager::new(session, 0);
        assert_eq!(manager.capacity(), 1);
        assert_eq!(manager.available(), 1);
    }

    #[test]
    fn acquire_without_handshake_yields_error() {
        let session = ssh2::Session::new().expect("session");
        let mut manager = MultiChannelSftpManager::new(session, 2);
        match manager.acquire() {
            Err(AcquireError::Create(_)) => {}
            Err(other) => panic!("unexpected error variant: {other:?}"),
            Ok(_) => panic!("expected acquire failure"),
        }
    }

    #[test]
    fn reset_refills_queue() {
        let session = ssh2::Session::new().expect("session");
        let mut manager = MultiChannelSftpManager::new(session, 3);
        // Even without successful acquire, reset should maintain queue sizing.
        manager.reset();
        assert_eq!(manager.available(), 3);
    }
}
