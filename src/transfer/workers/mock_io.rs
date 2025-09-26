#![allow(dead_code)]
use std::io::{self, Read, Write};

// PartialReader yields `first` bytes then returns an error on next read to simulate a remote read failure
pub struct PartialReader {
    data: Vec<u8>,
    pos: usize,
    fail_after_reads: usize,
    reads: usize,
}

impl PartialReader {
    pub fn new(data: &[u8], fail_after_reads: usize) -> Self {
        Self { data: data.to_vec(), pos: 0, fail_after_reads, reads: 0 }
    }
}

impl Read for PartialReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.reads >= self.fail_after_reads {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "simulated remote read failure",
            ));
        }
        if self.pos >= self.data.len() {
            self.reads += 1;
            return Ok(0);
        }
        let n = std::cmp::min(buf.len(), self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        self.reads += 1;
        Ok(n)
    }
}

// FailingWriter will accept some bytes then return an error on write to simulate remote write failure
pub struct FailingWriter {
    fail_after_writes: usize,
    writes: usize,
}

impl FailingWriter {
    pub fn new(fail_after_writes: usize) -> Self {
        Self { fail_after_writes, writes: 0 }
    }
}

impl Write for FailingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.writes >= self.fail_after_writes {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "simulated remote write failure",
            ));
        }
        self.writes += 1;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// Simple in-memory writer to capture writes
pub struct InMemoryWriter {
    pub data: std::sync::Mutex<Vec<u8>>,
}

impl InMemoryWriter {
    pub fn new() -> Self {
        Self { data: std::sync::Mutex::new(Vec::new()) }
    }
}

impl Write for InMemoryWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut guard = self.data.lock().unwrap();
        guard.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
