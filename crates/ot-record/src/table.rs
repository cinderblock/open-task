//! Tables of values shared by pointer between snapshots.
//!
//! The probe hands out a `ProcessStatic` (and a disk's, adapter's or GPU's facts, a
//! process's window and service list, the machine's service list) behind an `Arc`
//! and keeps the same `Arc` from pass to pass while the value is unchanged; a
//! change is a new allocation. So pointer identity is exactly "the same value as
//! last time", for every shared type alike, and the recorder writes each distinct
//! pointer once, under a small id, and refers to it from frames by that id.
//!
//! The writer holds a clone of every `Arc` it has numbered, so the address cannot be
//! freed and reused for a different value while it is in the table. Entries nothing
//! else holds any more (the process exited and no snapshot in flight has it) are
//! dropped on [`Writer::sweep`], so a long recording does not keep every process it
//! ever saw.
//!
//! The reader keeps one `Arc` per id and hands the same one to every frame that
//! refers to it, so `Arc::ptr_eq` means in a replay what it means live.

use std::collections::HashMap;
use std::sync::Arc;

use crate::Error;

/// The id of an empty slice, which is never written to a table.
pub(crate) const EMPTY: u32 = 0;

/// The writer's side: pointer to id, with the ids not yet written.
#[derive(Debug)]
pub(crate) struct Writer<T: ?Sized> {
    ids: HashMap<usize, (u32, Arc<T>)>,
    next: u32,
    pending: Vec<(u32, Arc<T>)>,
}

impl<T: ?Sized> Default for Writer<T> {
    fn default() -> Self {
        Self {
            ids: HashMap::new(),
            next: EMPTY + 1,
            pending: Vec::new(),
        }
    }
}

fn address<T: ?Sized>(a: &Arc<T>) -> usize {
    Arc::as_ptr(a).cast::<()>() as usize
}

impl<T: ?Sized> Writer<T> {
    /// The id for `value`, numbering it (and queueing it to be written) the first
    /// time this pointer is seen.
    pub(crate) fn id(&mut self, value: &Arc<T>) -> u32 {
        let addr = address(value);
        if let Some((id, _)) = self.ids.get(&addr) {
            return *id;
        }
        let id = self.next;
        self.next += 1;
        self.ids.insert(addr, (id, Arc::clone(value)));
        self.pending.push((id, Arc::clone(value)));
        id
    }

    /// The values numbered since the last call, to be written before the frame
    /// that refers to them.
    pub(crate) fn take_pending(&mut self) -> Vec<(u32, Arc<T>)> {
        std::mem::take(&mut self.pending)
    }

    /// Forget every value only this table still holds. Such a value cannot appear
    /// in a later snapshot by the same pointer: it has been freed, and whatever is
    /// allocated at that address next is numbered afresh. Call after
    /// [`Writer::take_pending`]: a pending clone would keep its entry alive.
    pub(crate) fn sweep(&mut self) {
        self.ids.retain(|_, (_, a)| Arc::strong_count(a) > 1);
    }

    /// Distinct values currently held.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.ids.len()
    }
}

impl<U> Writer<[U]> {
    /// Like [`Writer::id`], but an empty slice is [`EMPTY`] and is never written.
    pub(crate) fn id_or_empty(&mut self, value: &Arc<[U]>) -> u32 {
        if value.is_empty() {
            EMPTY
        } else {
            self.id(value)
        }
    }
}

/// The reader's side: id to one shared `Arc`.
#[derive(Debug)]
pub(crate) struct Reader<T: ?Sized> {
    items: HashMap<u32, Arc<T>>,
    /// What [`EMPTY`] stands for, on tables of slices.
    empty: Option<Arc<T>>,
}

impl<T: ?Sized> Default for Reader<T> {
    fn default() -> Self {
        Self {
            items: HashMap::new(),
            empty: None,
        }
    }
}

impl<U> Reader<[U]> {
    pub(crate) fn with_empty() -> Self {
        Self {
            items: HashMap::new(),
            empty: Some(Vec::new().into()),
        }
    }
}

impl<T: ?Sized> Reader<T> {
    /// Add the values of one table record.
    pub(crate) fn insert(&mut self, values: Vec<(u32, Arc<T>)>) {
        for (id, v) in values {
            self.items.insert(id, v);
        }
    }

    /// The value under `id`, shared with every other frame that refers to it.
    pub(crate) fn get(&self, id: u32, what: &'static str) -> Result<Arc<T>, Error> {
        if id == EMPTY {
            if let Some(e) = &self.empty {
                return Ok(Arc::clone(e));
            }
        }
        self.items
            .get(&id)
            .map(Arc::clone)
            .ok_or_else(|| Error::corrupt(format!("{what} {id} was never written")))
    }

    /// Distinct values held.
    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_pointer_same_id_new_pointer_new_id() {
        let mut w = Writer::<String>::default();
        let a = Arc::new("a".to_owned());
        let a2 = Arc::clone(&a);
        let b = Arc::new("a".to_owned());
        assert_eq!(w.id(&a), 1);
        assert_eq!(w.id(&a2), 1);
        assert_eq!(w.id(&b), 2);
        let pending = w.take_pending();
        assert_eq!(pending.len(), 2);
        assert_eq!(w.take_pending().len(), 0);
    }

    #[test]
    fn sweep_drops_only_what_nobody_else_holds() {
        let mut w = Writer::<String>::default();
        let kept = Arc::new("kept".to_owned());
        let gone = Arc::new("gone".to_owned());
        w.id(&kept);
        w.id(&gone);
        assert_eq!(w.take_pending().len(), 2);
        drop(gone);
        w.sweep();
        assert_eq!(w.len(), 1);
        assert_eq!(w.id(&kept), 1);
        // A new allocation, even at a reused address, gets a new id.
        let fresh = Arc::new("fresh".to_owned());
        assert_eq!(w.id(&fresh), 3);
    }

    #[test]
    fn empty_slices_share_one_id_and_one_arc() {
        let mut w = Writer::<[u8]>::default();
        let e1: Arc<[u8]> = Vec::new().into();
        let e2: Arc<[u8]> = Vec::new().into();
        assert_eq!(w.id_or_empty(&e1), EMPTY);
        assert_eq!(w.id_or_empty(&e2), EMPTY);
        assert_eq!(w.take_pending().len(), 0);
        let r = Reader::<[u8]>::with_empty();
        let a = r.get(EMPTY, "x").unwrap();
        let b = r.get(EMPTY, "x").unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert!(matches!(r.get(5, "x"), Err(Error::Corrupt(_))));
        assert!(matches!(
            Reader::<String>::default().get(EMPTY, "x"),
            Err(Error::Corrupt(_))
        ));
    }
}
