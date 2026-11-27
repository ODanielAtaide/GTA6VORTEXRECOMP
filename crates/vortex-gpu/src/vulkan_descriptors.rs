// SPDX-License-Identifier: GPL-3.0-or-later
//! Backend-neutral descriptor update journal. The caller owns Vulkan objects
//! and must wait for all users of a descriptor set before updating it.
//! Resource identity includes generation, so reused handles cannot alias.
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferBinding {
    pub resource: u64,
    pub generation: u64,
    pub offset: u64,
    pub range: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalError {
    InvalidSlot,
    InvalidRange,
    Pending,
    NoPending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Write {
    pub slot: usize,
    pub binding: BufferBinding,
}

/// One fixed-size set of storage buffer bindings. Slots start unbound.
/// Unbinding is intentionally unsupported: Vulkan null descriptors require
/// a separately negotiated feature. Use an explicit valid dummy buffer.
pub struct Journal {
    desired: Vec<Option<BufferBinding>>,
    published: Vec<Option<BufferBinding>>,
    dirty: BTreeSet<usize>,
    pending: Option<Vec<Write>>,
}

impl Journal {
    pub fn new(slots: usize) -> Self {
        Self {
            desired: vec![None; slots],
            published: vec![None; slots],
            dirty: BTreeSet::new(),
            pending: None,
        }
    }

    /// Record a CPU-side descriptor change. Repeated identical writes and
    /// changes reverted before publication do not cause Vulkan updates.
    pub fn set(&mut self, slot: usize, binding: BufferBinding) -> Result<(), JournalError> {
        if slot >= self.desired.len() { return Err(JournalError::InvalidSlot); }
        if binding.range == 0 || binding.offset.checked_add(binding.range).is_none() {
            return Err(JournalError::InvalidRange);
        }
        self.desired[slot] = Some(binding);
        if self.desired[slot] != self.published[slot] {
            self.dirty.insert(slot);
        } else {
            self.dirty.remove(&slot);
        }
        Ok(())
    }

    /// Snapshot only dirty entries, sorted by slot. Changes after this
    /// snapshot remain eligible for the next publication.
    pub fn begin(&mut self) -> Result<&[Write], JournalError> {
        if self.pending.is_some() { return Err(JournalError::Pending); }
        self.pending = Some(self.dirty.iter().map(|&slot| Write {
            slot,
            binding: self.desired[slot].expect("dirty slots are bound"),
        }).collect());
        Ok(self.pending.as_deref().expect("just set"))
    }

    /// Acknowledge successful application of the exact pending snapshot.
    /// On failure, preserve all entries that still differ from publication.
    pub fn finish(&mut self, success: bool) -> Result<(), JournalError> {
        let writes = self.pending.take().ok_or(JournalError::NoPending)?;
        for w in writes {
            if success { self.published[w.slot] = Some(w.binding); }
            if self.desired[w.slot] != self.published[w.slot] {
                self.dirty.insert(w.slot);
            } else {
                self.dirty.remove(&w.slot);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binding(resource: u64, generation: u64) -> BufferBinding {
        BufferBinding { resource, generation, offset: 0, range: 4 }
    }
    #[test]
    fn deduplicates_and_reverts() {
        let mut j = Journal::new(8);
        j.set(3,binding(1,1)).unwrap();
        j.set(3,binding(1,1)).unwrap();
        assert_eq!(j.begin().unwrap().len(),1);
        j.finish(true).unwrap();
        j.set(3,binding(2,1)).unwrap();
        j.set(3,binding(1,1)).unwrap();
        assert!(j.begin().unwrap().is_empty());
        j.finish(true).unwrap();
    }
    #[test]
    fn newer_change_survives_publication() {
        let mut j = Journal::new(8);
        j.set(0,binding(1,1)).unwrap();
        j.begin().unwrap();
        j.set(0,binding(2,1)).unwrap();
        j.finish(true).unwrap();
        assert_eq!(j.begin().unwrap(), &[Write { slot: 0,binding: binding(2,1) }]);
        j.finish(true).unwrap();
    }
    #[test]
    fn reverted_to_old_binding_during_publication() {
        let mut j = Journal::new(1);
        j.set(0,binding(1,1)).unwrap();
        j.begin().unwrap(); j.finish(true).unwrap();
        j.set(0,binding(2,1)).unwrap();
        j.begin().unwrap();
        j.set(0,binding(1,1)).unwrap();
        j.finish(true).unwrap();
        assert_eq!(j.begin().unwrap()[0].binding,binding(1,1));
        j.finish(true).unwrap();
    }
    #[test]
    fn generation_and_failed_update() {
        let mut j = Journal::new(1);
        j.set(0,binding(1,1)).unwrap();
        j.begin().unwrap(); j.finish(true).unwrap();
        j.set(0,binding(1,2)).unwrap();
        assert_eq!(j.begin().unwrap().len(),1);
        j.finish(false).unwrap();
        assert_eq!(j.begin().unwrap()[0].binding,binding(1,2));
        j.finish(true).unwrap();
    }
    #[test]
    fn validates_ranges_and_publication_order() {
        let mut j = Journal::new(1);
        assert_eq!(j.set(1,binding(1,1)),Err(JournalError::InvalidSlot));
        assert_eq!(j.set(0,BufferBinding { offset: u64::MAX,..binding(1,1) }),Err(JournalError::InvalidRange));
        assert_eq!(j.finish(true),Err(JournalError::NoPending));
        j.begin().unwrap();
        assert_eq!(j.begin().unwrap_err(),JournalError::Pending);
        j.finish(false).unwrap();
    }
    #[test]
    fn sparse_600000_slots() {
        let mut j = Journal::new(600_000);
        for i in [599_999,32,0,7000,32] {
            j.set(i,binding(i as u64,1)).unwrap();
        }
        let batch = j.begin().unwrap();
        assert_eq!(batch.iter().map(|w| w.slot).collect::<Vec<_>>(),vec![0,32,7000,599_999]);
        println!("planned {} descriptor writes of 600000 slots",batch.len());
        j.finish(true).unwrap();
        assert!(j.begin().unwrap().is_empty());
        j.finish(true).unwrap();
    }
}
