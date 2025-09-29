use std::io::Read;

use crossbeam_channel::Sender;

/// Message sent from reader thread to writer loop
pub enum ReadMsg {
    Data(Vec<u8>),
    Err(String),
    Eof,
}

/// Spawn a reader thread that reads from `reader` with chunks of `chunk_size` and
/// sends `ReadMsg` to `tx`. Returns the JoinHandle.
pub fn spawn_file_reader<R: Read + Send + 'static>(
    mut reader: R,
    tx: Sender<ReadMsg>,
    chunk_size: usize,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        loop {
            let mut buf = vec![0u8; chunk_size];
            match reader.read(&mut buf) {
                Ok(0) => {
                    let _ = tx.send(ReadMsg::Eof);
                    break;
                }
                Ok(n) => {
                    buf.truncate(n);
                    if tx.send(ReadMsg::Data(buf)).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(ReadMsg::Err(format!("reader error: {}", e)));
                    break;
                }
            }
        }
    })
}

/// Compute a new buffer size based on bytes written and total write duration.
/// Returns clamped value between min_size and max_size.
pub fn adapt_buf_size(
    current: usize,
    bytes_written: u64,
    write_duration: std::time::Duration,
    target_ms: f64,
    min_size: usize,
    max_size: usize,
) -> usize {
    if write_duration.as_secs_f64() <= 0.0 || bytes_written == 0 {
        return current;
    }
    let bps = (bytes_written as f64) / write_duration.as_secs_f64();
    let desired = (bps * (target_ms / 1000.0)) as usize;
    std::cmp::max(min_size, std::cmp::min(max_size, desired))
}

/// Pipeline defaults.
#[derive(Debug, Clone, Copy)]
pub struct PipelineConfig {
    pub enable_min: u64,
    pub depth: usize,
    pub target_ms: f64,
    pub min_size: usize,
    pub max_size: usize,
}

impl PipelineConfig {
    pub fn defaults() -> Self {
        Self {
            enable_min: 128 * 1024, // 128 KiB
            depth: 4,
            target_ms: 150.0,
            min_size: 64 * 1024,
            max_size: 8 * 1024 * 1024,
        }
    }

    pub fn current() -> Self {
        Self::defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::bounded;
    use std::io::Cursor;

    #[test]
    fn adapt_buf_size_clamps() {
        let cur = 65536usize;
        let new = adapt_buf_size(
            cur,
            1024 * 1024,
            std::time::Duration::from_millis(100),
            150.0,
            64 * 1024,
            8 * 1024 * 1024,
        );
        assert!((64 * 1024..=8 * 1024 * 1024).contains(&new));
    }

    #[test]
    fn spawn_file_reader_sends_data_and_eof() {
        let data = b"hello world".to_vec();
        let (tx, rx) = bounded::<ReadMsg>(2);
        let cursor = Cursor::new(data.clone());
        let h = spawn_file_reader(cursor, tx, 4);
        let mut collected = Vec::new();
        loop {
            match rx.recv().expect("recv") {
                ReadMsg::Data(v) => collected.extend_from_slice(&v),
                ReadMsg::Err(e) => panic!("reader error: {}", e),
                ReadMsg::Eof => break,
            }
        }
        let _ = h.join();
        assert_eq!(collected, data);
    }
}
