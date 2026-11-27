//! Backend-independent dirty upload planning for the future Vulkan backend.
//!
//! Inspired by the selective memory updates documented in Belobug's
//! docs/findings.md; this implementation is original. No Vulkan objects or
//! D3D12 objects are owned here. One submission may be in flight per tracker.
//! The caller must synchronize CPU writes and staging snapshots separately.

use std::ops::Range;

/// The console's memory tracking granularity.
pub const PAGE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadError {
    EmptyBuffer,
    OutOfBounds,
    SubmissionInFlight,
    NoSubmission,
    EpochExhausted,
}

#[derive(Debug)]
struct Pending {
    epoch: u64,
    ranges: Vec<Range<usize>>,
}

/// Tracks one buffer's dirty pages across asynchronous upload completion.
/// New buffers start dirty. Clean pages have epoch zero.
#[derive(Debug)]
pub struct DirtyUpload {
    bytes: usize,
    epoch: u64,
    dirty: Vec<u64>,
    pending: Option<Pending>,
}

impl DirtyUpload {
    pub fn new(bytes: usize) -> Result<Self, UploadError> {
        if bytes == 0 {
            return Err(UploadError::EmptyBuffer);
        }
        Ok(Self {
            bytes,
            epoch: 1,
            dirty: vec![1; bytes.div_ceil(PAGE_BYTES)],
            pending: None,
        })
    }

    /// Report a CPU write, even while an older upload is in flight.
    pub fn mark(&mut self, offset: usize, len: usize) -> Result<(), UploadError> {
        let end = offset.checked_add(len).ok_or(UploadError::OutOfBounds)?;
        if end > self.bytes {
            return Err(UploadError::OutOfBounds);
        }
        if len == 0 {
            return Ok(());
        }
        self.epoch = self.epoch.checked_add(1).ok_or(UploadError::EpochExhausted)?;
        self.dirty[offset / PAGE_BYTES..=(end - 1) / PAGE_BYTES].fill(self.epoch);
        Ok(())
    }

    /// Snapshot and coalesce dirty pages into non-overlapping byte ranges.
    /// The caller copies these ranges to staging memory before submission.
    /// Do not submit empty plans. Each successful begin requires finish.
    pub fn begin(&mut self) -> Result<&[Range<usize>], UploadError> {
        if self.pending.is_some() {
            return Err(UploadError::SubmissionInFlight);
        }
        let mut ranges = Vec::new();
        let mut i = 0;
        while i < self.dirty.len() {
            if self.dirty[i] == 0 {
                i += 1;
                continue;
            }
            let first = i;
            while i < self.dirty.len() && self.dirty[i] != 0 {
                i += 1;
            }
            let end = i.saturating_mul(PAGE_BYTES).min(self.bytes);
            ranges.push(first * PAGE_BYTES..end);
        }
        self.pending = Some(Pending { epoch: self.epoch, ranges });
        Ok(self.pending.as_ref().expect("just set").ranges.as_slice())
    }

    /// Call only after the corresponding GPU timeline/fence confirms success,
    /// or with false on cancellation/failure. Writes newer than this snapshot
    /// remain dirty. The caller must not release in-flight Vulkan resources.
    pub fn finish(&mut self, success: bool) -> Result<(), UploadError> {
        let pending = self.pending.take().ok_or(UploadError::NoSubmission)?;
        if success {
            for range in pending.ranges {
                for epoch in &mut self.dirty[range.start / PAGE_BYTES..range.end.div_ceil(PAGE_BYTES)] {
                    if *epoch <= pending.epoch {
                        *epoch = 0;
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_upload_then_no_changes() {
        let mut d = DirtyUpload::new(3 * PAGE_BYTES + 7).unwrap();
        assert_eq!(d.begin().unwrap(), &[0..3 * PAGE_BYTES + 7]);
        d.finish(true).unwrap();
        assert!(d.begin().unwrap().is_empty());
        d.finish(true).unwrap();
    }

    #[test]
    fn coalesces_adjacent_pages_and_clamps_tail() {
        let p = PAGE_BYTES;
        let mut d = DirtyUpload::new(5*p+12).unwrap();
        d.begin().unwrap();
        d.finish(true).unwrap();
        d.mark(p-1, 2).unwrap();
        d.mark(5*p+11, 1).unwrap();
        assert_eq!(d.begin().unwrap(), &[0..2*p, 5*p..5*p+12]);
        d.finish(true).unwrap();
    }

    #[test]
    fn write_during_upload_is_not_lost() {
        let p = PAGE_BYTES;
        let mut d = DirtyUpload::new(3*p).unwrap();
        d.begin().unwrap();
        d.mark(p+1, 1).unwrap();
        d.finish(true).unwrap();
        assert_eq!(d.begin().unwrap(), &[p..2*p]);
        d.finish(true).unwrap();
        assert!(d.begin().unwrap().is_empty());
        d.finish(true).unwrap();
    }

    #[test]
    fn failed_upload_keeps_all_dirty_pages() {
        let p = PAGE_BYTES;
        let mut d = DirtyUpload::new(4*p).unwrap();
        d.begin().unwrap();
        d.finish(true).unwrap();
        d.mark(0, 1).unwrap();
        d.begin().unwrap();
        d.mark(3*p, 1).unwrap();
        d.finish(false).unwrap();
        assert_eq!(d.begin().unwrap(), &[0..p, 3*p..4*p]);
        d.finish(true).unwrap();
    }

    #[test]
    fn invalid_ranges_and_duplicate_submissions_are_rejected() {
        assert_eq!(DirtyUpload::new(0).unwrap_err(), UploadError::EmptyBuffer);
        let mut d = DirtyUpload::new(PAGE_BYTES).unwrap();
        assert_eq!(d.mark(usize::MAX, 1), Err(UploadError::OutOfBounds));
        assert_eq!(d.mark(PAGE_BYTES, 1), Err(UploadError::OutOfBounds));
        assert_eq!(d.mark(PAGE_BYTES, 0), Ok(()));
        assert_eq!(d.finish(true), Err(UploadError::NoSubmission));
        d.begin().unwrap();
        assert_eq!(d.begin().unwrap_err(), UploadError::SubmissionInFlight);
        d.finish(false).unwrap();
    }

    #[test]
    fn sparse_upload_budget() {
        let total = 256 * 1024 * 1024;
        let mut d = DirtyUpload::new(total).unwrap();
        d.begin().unwrap();
        d.finish(true).unwrap();
        for i in 0..16 {
            d.mark(i * 16 * 1024 * 1024 + 1, 4).unwrap();
        }
        let ranges = d.begin().unwrap();
        assert_eq!(ranges.len(), 16);
        let planned: usize = ranges.iter().map(Range::len).sum();
        assert_eq!(planned, 1024 * 1024);
        println!("planned {planned} bytes of {total} bytes");
        d.finish(true).unwrap();
    }
}
