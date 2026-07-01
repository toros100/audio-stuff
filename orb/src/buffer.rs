#[cfg(not(loom))]
use std::sync::{
    Arc,
    atomic::{AtomicU32, AtomicUsize},
};

#[cfg(loom)]
use loom::sync::{
    Arc,
    atomic::{AtomicU32, AtomicUsize},
};

// # push behaviour spec
//
// if seq is way off:
// set everything to clear, set expected_seq even if we can't push it
// why do that unconditionally?
// because we just expect that to be the new time basis?
//
// if set is a bit off:
//
// seq in the past:
// try to insert into placeholder, if one exists, otherwise what?
// discard value? or return value?
//
// there is not really any point in returning the value? it will never be pushable?
//
// seq in the present-future:
//
// insert
//

use crate::shared_array::SharedArray;

use crossbeam_utils::CachePadded;

struct Shared<T, const SIZE: usize> {
    read_head: CachePadded<AtomicUsize>,
    write_head: CachePadded<AtomicUsize>,
    arr: SharedArray<T, SIZE>,
    cells: [AtomicU32; SIZE], // will contain CellState variants packed into u32
}

impl<const N: usize, T> Default for Shared<T, N> {
    fn default() -> Self {
        let read_head = CachePadded::new(AtomicUsize::new(0));
        let write_head = CachePadded::new(AtomicUsize::new(0));

        let skip = CellState::Skip.pack();
        let cells = std::array::from_fn(|_| AtomicU32::new(skip));
        let arr = SharedArray::default();
        Self {
            arr,
            read_head,
            write_head,
            cells,
        }
    }
}

pub struct Writer<T, const N: usize> {
    shared: Arc<Shared<T, N>>,
    write_head_local: usize,
    expected_seq: u16,
}

pub struct Reader<T, const N: usize> {
    shared: Arc<Shared<T, N>>,
    read_head_local: usize,
}

unsafe impl<T: Send, const N: usize> Send for Writer<T, N> {}
unsafe impl<T: Send, const N: usize> Send for Reader<T, N> {}

pub fn new_pair<T, const N: usize>() -> (Writer<T, N>, Reader<T, N>) {
    let shared = Arc::new(Shared::<T, N>::default());

    let w = Writer {
        shared: shared.clone(),
        write_head_local: 0,
        expected_seq: 0,
    };

    let r = Reader {
        shared,
        read_head_local: 0,
    };
    (w, r)
}

#[derive(Debug, Clone, Copy)]
pub enum PopResult<T> {
    Empty,
    Missing(u16),
    Data(u16, T),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CellState {
    Skip,
    Reserved(u16),
    Data(u16),
}

impl CellState {
    const SKIP: u16 = 0;
    const NO_DATA: u16 = 1;
    const WITH_DATA: u16 = 2;

    fn unpack(val: u32) -> Self {
        // 16 least significant bits are a u16 sequence number
        let seq = (val & 0xFFFF) as u16;

        // the remaining bits are used as a discriminator
        let v = (val >> 16u16) as u16;

        match v {
            Self::NO_DATA => CellState::Reserved(seq),
            Self::SKIP => CellState::Skip,
            Self::WITH_DATA => CellState::Data(seq),
            _ => unreachable!(),
        }
    }

    fn pack(self) -> u32 {
        match self {
            Self::Skip => (Self::SKIP as u32) << 16,
            Self::Reserved(seq) => (Self::NO_DATA as u32) << 16 | (seq as u32),
            Self::Data(seq) => (Self::WITH_DATA as u32) << 16 | (seq as u32),
        }
    }
}

fn wrapping_range_u16(start: u16, end: u16) -> impl DoubleEndedIterator<Item = u16> {
    let n = end.wrapping_sub(start);
    (0u16..n).map(move |i| start.wrapping_add(i))
}

fn wrapping_range_usize(start: usize, end: usize) -> impl DoubleEndedIterator<Item = usize> {
    let n = end.wrapping_sub(start);
    (0usize..n).map(move |i| start.wrapping_add(i))
}

fn wrapping_range_usize_contains(start: usize, end: usize, val: usize) -> bool {
    val.wrapping_sub(start) < end.wrapping_sub(start)
}

#[derive(thiserror::Error, Debug)]
pub enum PushError<T> {
    Stale(u16, T),
    Full(u16, T),
}

impl<T, const N: usize> Writer<T, N> {
    const JUMP_THRESH: u16 = {
        if N > 0xFFFF { 0xFFFFu16 } else { N as u16 }
        // NOTE: kind of arbitrary
    };

    // seq far away from self.expected_seq
    // need this to support randomized initial sequence numbers (which RTP does for security reasons),
    // but also to better deal with a huge burst of lost packets
    fn should_jump_to(&self, seq: u16) -> bool {
        // true iff seq is "outside" the wrapping range from self.expected_seq.wrapping_sub(Self::JUMP_THRESH) to
        // self.expected_seq.wrapping_add(Self::JUMP_THRESH) (inclusive)

        let start = self.expected_seq.wrapping_sub(Self::JUMP_THRESH);
        let end = self.expected_seq.wrapping_add(Self::JUMP_THRESH);

        seq.wrapping_sub(start) >= end.wrapping_sub(start)
    }

    fn diff(&self, seq: u16) -> i16 {
        seq.wrapping_sub(self.expected_seq) as i16
    }

    fn ass(&self) {
        debug_assert_eq!(
            self.write_head_local,
            self.shared
                .write_head
                .load(std::sync::atomic::Ordering::Relaxed)
        )
    }

    pub fn push(&mut self, seq: u16, data: T) -> Result<(), PushError<T>> {
        const {
            assert!(N.is_power_of_two());
            assert!(Self::JUMP_THRESH > 0);
            assert!((Self::JUMP_THRESH as usize) <= N);
        };

        // TODO: rewrite this bs

        debug_assert_eq!(
            self.write_head_local,
            self.shared
                .write_head
                .load(std::sync::atomic::Ordering::Relaxed)
        );

        // need to use Acquire ordering here s.t. the writers "take" operations on the shared array
        // are available here. (reader stores its new position with Release ordering after taking a
        // value)
        let read = self
            .shared
            .read_head
            .load(std::sync::atomic::Ordering::Acquire);

        if self.should_jump_to(seq) {
            // seq is quite far ahead or behind compared to self.expected_seq
            // we will essentially flush the buffer and take seq as the base of our new expected
            // "timeline"

            let skip = CellState::Skip.pack();
            for i in wrapping_range_usize(read, self.write_head_local).rev() {
                let idx = i % N;
                // NOTE: we can NOT safely remove values in self.arr at idx at this point, because the
                // reader could be accessing the same index concurrently, in case we overwrote
                // Data(_) with Skip.
                // this could be solved with additional synchronization, but we will just drop any
                // leftover values after the reader advances past the corresponding index again.

                // reversed the iterator and used Release ordering to make the "skipped" area more
                // coherent
                self.shared.cells[idx].store(skip, std::sync::atomic::Ordering::Release);
            }

            self.expected_seq = seq.wrapping_add(1);

            if self.write_head_local.wrapping_sub(N) == read {
                self.ass();
                // buffer is full
                Err(PushError::Full(seq, data))
            } else {
                let idx = self.write_head_local % N;

                // SAFETY:
                // ...
                unsafe {
                    self.shared.arr.insert(idx, data);
                };

                // Release order is important here, so the write to the array is
                // visible to the reader after seeing Data(_)
                self.shared.cells[idx].store(
                    CellState::Data(seq).pack(),
                    std::sync::atomic::Ordering::Release,
                );

                self.write_head_local = self.write_head_local.wrapping_add(1);

                self.shared
                    .write_head
                    .store(self.write_head_local, std::sync::atomic::Ordering::Release);

                self.ass();

                Ok(())
            }
        } else {
            let d = self.diff(seq);
            if d < 0 {
                // seq is past sequence number, we try to insert the data if the reader has not
                // already advanced past the position
                let lookback = d.unsigned_abs() as usize;
                let pos = self.write_head_local.wrapping_sub(lookback);
                if wrapping_range_usize_contains(read, self.write_head_local, pos) {
                    let idx = pos % N;
                    let expected_state = CellState::Reserved(seq).pack();

                    if self.shared.cells[idx].load(std::sync::atomic::Ordering::Acquire)
                        == expected_state
                    {
                        // SAFETY:
                        // state was CellState::Reserved(_), thus the reader is not accessing
                        // the shared array at idx
                        // (no need to do CAS here, because the writer is the only one modifying
                        // cell state)
                        unsafe {
                            self.shared.arr.insert(idx, data);
                        }

                        // Release order is important here, so the write to the array is
                        // visible to the reader after seeing Data(_)
                        self.shared.cells[idx].store(
                            CellState::Data(seq).pack(),
                            std::sync::atomic::Ordering::Release,
                        );
                        return Ok(());
                    }
                }
                Err(PushError::Stale(seq, data))
            } else {
                // seq is the expected sequence number or a future sequence number

                // inserting placeholders for skipped sequence numbers (if any)
                for s in wrapping_range_u16(self.expected_seq, seq) {
                    // note that the range does not include seq itself

                    if self.write_head_local == read.wrapping_add(N) {
                        break;
                    } else {
                        let val = CellState::Reserved(s).pack();
                        let idx = self.write_head_local % N;

                        // SAFETY:
                        // since we excluded read_head.wrapping_add(N) == write_head, the only
                        // way to have idx != read_head % N would be read_head == write_head
                        // but then the buffer would be empty, and the reader would not read anything
                        unsafe {
                            self.shared.arr.drop_if_present(idx);
                        }

                        // no need to impose ordering here, after seeing Reserved(_) the reader
                        // will not touch the corresponding shared array index
                        // TODO: think about reversed iteration + release ordering here
                        self.shared.cells[idx].store(val, std::sync::atomic::Ordering::Relaxed);
                        self.write_head_local = self.write_head_local.wrapping_add(1);
                        self.expected_seq = s.wrapping_add(1)
                    }
                }

                self.shared
                    .write_head
                    .store(self.write_head_local, std::sync::atomic::Ordering::Release);

                let ret = if self.write_head_local != read.wrapping_add(N) {
                    let idx = self.write_head_local % N;
                    let val = CellState::Data(seq).pack();

                    // SAFETY:
                    // since we excluded read_head.wrapping_add(N) == write_head, the only
                    // way to have idx != read_head % N would be read_head == write_head
                    // but then the buffer would be empty, and the reader would not read anything
                    unsafe {
                        self.shared.arr.insert(idx, data);
                    }

                    // Release order is important here, so the write to the array is
                    // visible to the reader after seeing Data(_)
                    self.shared.cells[idx].store(val, std::sync::atomic::Ordering::Release);
                    self.write_head_local = self.write_head_local.wrapping_add(1);
                    Ok(())
                } else {
                    Err(PushError::Full(seq, data))
                };

                // Release is required here after all
                if ret.is_ok() {
                    self.shared
                        .write_head
                        .store(self.write_head_local, std::sync::atomic::Ordering::Release);
                    self.ass();
                }

                if ret.is_ok() {
                    // i hate this
                    self.expected_seq = seq.wrapping_add(1);
                }
                ret
            }
        }
    }
}

impl<T, const N: usize> Reader<T, N> {
    /// upper bound on the number of available items
    pub fn len_hint(&self) -> usize {
        // this is not necessarily the exact number of available items, because some readable
        // indices might have CellState::Skip
        self.shared
            .write_head
            .load(std::sync::atomic::Ordering::Relaxed)
            .wrapping_sub(self.read_head_local)
    }

    pub fn is_empty(&self) -> bool {
        self.len_hint() == 0
    }

    pub fn pop(&mut self) -> PopResult<T> {
        debug_assert_eq!(
            self.read_head_local,
            self.shared
                .read_head
                .load(std::sync::atomic::Ordering::Relaxed)
        );

        loop {
            // loop instead of recursion to avoid unbounded recursion on pathological inputs

            let write_head = self
                .shared
                .write_head
                .load(std::sync::atomic::Ordering::Acquire);

            if self.read_head_local == write_head {
                break PopResult::Empty;
            }

            let idx = self.read_head_local % N;
            // Acquire ordering is important here: after writing to the shared array at this index,
            // the writer will have stored this cell state with Release ordering
            let state = self.shared.cells[idx].load(std::sync::atomic::Ordering::Acquire);

            match CellState::unpack(state) {
                CellState::Skip => {
                    self.read_head_local = self.read_head_local.wrapping_add(1);
                    self.shared
                        .read_head
                        .store(self.read_head_local, std::sync::atomic::Ordering::Relaxed);
                    // could maybe drop_if_present here to get stale values in the shared array
                    // dropped a tiny bit faster? but would need to consider ordering here, and the
                    // values will be dropped by the writer in any case
                    continue;
                }
                CellState::Reserved(seq) => {
                    self.read_head_local = self.read_head_local.wrapping_add(1);
                    self.shared
                        .read_head
                        .store(self.read_head_local, std::sync::atomic::Ordering::Release);
                    break PopResult::Missing(seq);
                }
                CellState::Data(seq) => {
                    // SAFETY:
                    // until the reader has advanced past this index again, the only thing the writer
                    // could do is to update the cell state to CellState::Skip. but even in this
                    // case, the writer is not actually touching the shared array at that index,
                    // until the reader has advanced past it, so the following access is still safe,
                    // even if the cell state got updated since we loaded it
                    let data = unsafe { self.shared.arr.take(idx) };
                    self.read_head_local = self.read_head_local.wrapping_add(1);

                    // need release ordering here, because "take" mutates the shared array
                    // (setting liveness bool to false), thus we need some ordering constraint here
                    // to ensure the writer sees this update when calling drop_if_present
                    self.shared
                        .read_head
                        .store(self.read_head_local, std::sync::atomic::Ordering::Release);
                    break PopResult::Data(seq, data);
                }
            }
        }
    }
}

#[cfg(test)]
#[cfg(not(loom))]
mod tests {

    use super::*;
    use std::assert_matches;

    #[test]
    fn cell_state_pack_unpack() {
        let c = CellState::Skip;
        assert_eq!(c, CellState::unpack(c.pack()));

        let c = CellState::Reserved(0);
        assert_eq!(c, CellState::unpack(c.pack()));

        let c = CellState::Reserved(u16::MAX);
        assert_eq!(c, CellState::unpack(c.pack()));

        let c = CellState::Data(0);
        assert_eq!(c, CellState::unpack(c.pack()));

        let c = CellState::Data(u16::MAX);
        assert_eq!(c, CellState::unpack(c.pack()));
    }

    #[test]
    fn basic() {
        let (mut tx, mut rx) = new_pair::<u64, 16>();

        let res = rx.pop();
        assert_matches!(res, PopResult::Empty);

        assert!(tx.push(0, 17).is_ok());

        let res2 = rx.pop();

        assert_matches!(res2, PopResult::Data(0, 17));
    }

    #[test]
    fn multiple() {
        let (mut tx, mut rx) = new_pair::<u64, 16>();

        for i in 0..16u16 {
            assert!(tx.push(i, i as u64).is_ok());
        }

        for i in 0..16u16 {
            let res = rx.pop();

            let _exp = PopResult::Data(i, i as u64);
            assert_matches!(res, _exp);
        }
    }

    #[test]
    fn gaps() {
        let (mut tx, mut rx) = new_pair::<u64, 16>();

        for i in 0..16u16 {
            if i.is_multiple_of(2) {
                assert!(tx.push(i, i as u64).is_ok());
            }
        }

        for i in 0..16u16 {
            let res = rx.pop();

            if i.is_multiple_of(2) {
                let _exp = PopResult::Data(i, i as u64);
                assert_matches!(res, _exp);
            } else {
                let _exp = PopResult::<u64>::Missing(i);
                assert_matches!(res, _exp);
            }
        }
    }

    #[test]
    fn late_arrival_insertion() {
        let (mut tx, mut rx) = new_pair::<u64, 16>();

        assert!(tx.push(0, 0).is_ok());
        assert!(tx.push(2, 2).is_ok());
        assert!(tx.push(1, 1).is_ok());

        for i in 0..=3u16 {
            let res = rx.pop();

            let _exp = PopResult::Data(i, i as u64);
            assert_matches!(res, _exp);
        }
    }

    #[test]
    fn late_arrival_drop_placeholder() {
        let (mut tx, mut rx) = new_pair::<u64, 16>();

        for i in 0..=14u16 {
            assert!(tx.push(i, i as u64).is_ok());
        }

        // at capacity 15/16, room for just one more
        // currently have data with seq in 0..=14
        //
        // if we push data with seq 16 now, it should insert the placeholder for 15, but 16 will not
        // fit and be returned by the push. it might seem a bit odd to discard/reject actual data to
        // just keep a placeholder for data that MIGHT still arrive, but i think this is the correct
        // choice specifically for opus audio decoding:
        // whether 15 actually arrives or not, opus PLC is good enough that even 2 missing frames
        // is probably subjectively preferable to one actual hard discontinuity caused by going from
        // frame 14 straight to 16.

        let res = tx.push(16, 16);
        assert_matches!(res, Err(PushError::Full(16u16, 16u64)));

        for i in 0..14u16 {
            assert_eq!(
                tx.shared.cells[i as usize].load(std::sync::atomic::Ordering::Relaxed),
                CellState::Data(i).pack()
            );
        }

        assert_eq!(
            tx.shared.cells[15].load(std::sync::atomic::Ordering::Relaxed),
            CellState::Reserved(15).pack()
        );

        for i in 0..=15u16 {
            let res = rx.pop();
            let _exp = PopResult::Data(i, i as u64);
            assert_matches!(res, _exp);
        }

        let res = rx.pop();
        let _exp = PopResult::<u64>::Empty;
        assert_matches!(res, _exp);
    }

    #[test]
    fn full() {
        let (mut tx, mut rx) = new_pair::<u64, 16>();

        for i in 0..16u16 {
            match tx.push(i, i as u64) {
                Ok(()) => {}
                Err(e) => panic!("expected no push error, found {:?}", e),
            }
        }

        let res = tx.push(16u16, 16u64);
        assert_matches!(res, Err(PushError::Full(16u16, 16u64)));

        for i in 0..16u16 {
            let res = rx.pop();

            let _exp = PopResult::Data(i, i as u64);
            assert_matches!(res, _exp);
        }
    }
}

#[cfg(test)]
#[cfg(not(loom))]
mod drop_tests {

    use super::*;

    use std::assert_matches;
    use std::fmt::Debug;
    use std::ptr::addr_of;

    struct DummyHandle(*mut AtomicUsize);

    impl Drop for DummyHandle {
        fn drop(&mut self) {
            _ = unsafe { Box::from_raw(self.0) };
        }
    }

    impl DummyHandle {
        fn load_drop_count(&self) -> usize {
            unsafe { (*self.0).load(std::sync::atomic::Ordering::Relaxed) }
        }
    }

    #[derive(Debug)]
    enum DropDummy {
        Tracked(*const AtomicUsize),
        Untracked,
    }

    impl DropDummy {
        // creates a new "tracked" dummy: the DummyHandle may be checked to see how many times
        // the dummy was dropped. (this can obviously only happen if the dummy is duplicated using
        // unsafe, which is exactly what we are trying to test with this)
        // WARN: the returned DummyHandle must outlive the dummy (and any copies)
        fn new_tracked() -> (Self, DummyHandle) {
            let bx = Box::new(AtomicUsize::new(0));
            let ptr = Box::into_raw(bx);
            let dropped_count = ptr as *const AtomicUsize;
            (DropDummy::Tracked(dropped_count), DummyHandle(ptr))
        }
        // creates an "untracked" dummy: this is so that we can push items of the same type (the
        // enum type) into the buffer without having to juggle additional handles
        fn new_untracked() -> Self {
            DropDummy::Untracked
        }
    }

    // fine to send, just keep the handle alive
    unsafe impl Send for DropDummy {}

    impl Drop for DropDummy {
        fn drop(&mut self) {
            if let DropDummy::Tracked(dropped_count) = self {
                // this is the cleanest way i could come up with to safely be able to drop
                // multiple unsafe copies of the same value, while being able to track it.
                // as long as the corresponding DummyHandle is kept alive, this should be
                // safe and allow testing multiple drops without causing any ub or actually
                // corrupting the heap (which tends to just completely abort the test harness)
                _ = unsafe {
                    (*(*dropped_count)).fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                };
            }
        }
    }

    #[test]
    fn drop_dummy_sanity_check() {
        let (dd, h) = DropDummy::new_tracked();

        assert_eq!(h.load_drop_count(), 0);

        {
            let ptr = addr_of!(dd);
            let _illegal_duplicate = unsafe { ptr.read() };
        }

        assert_eq!(h.load_drop_count(), 1);

        drop(dd);

        assert_eq!(h.load_drop_count(), 2);
    }

    #[test]
    fn drop_exactly_once() {
        fn check_and_count_drops(bxs: &[(usize, DummyHandle)]) -> usize {
            bxs.iter()
                .map(|(id, bx)| {
                    let ct = bx.load_drop_count();

                    if ct > 1 {
                        panic!("DropDummy {} was dropped {} times", id, ct);
                    }

                    ct
                })
                .sum()
        }

        let mut handles = Vec::new();

        // WARN: it's important that handles is declared first, to ensure that it is dropped after
        // the tx/rx are, because the drop impl of SharedArray will drop any remaining
        // DropDummy values, which will dereference a pointer to an AtomicUsize that is dropped
        // when the DummyHandle is dropped.
        // (should not happen if the test behaves as expected, but there should still not be any ub
        // just because a test went wrong)

        let (mut tx, mut rx) = new_pair::<DropDummy, 64>();

        // inserting 64 items
        for i in 0..64usize {
            let (dd, h) = DropDummy::new_tracked();
            handles.push((i, h));
            assert!(tx.push(i as u16, dd).is_ok());
        }

        assert_eq!(rx.len_hint(), 64);
        assert_eq!(check_and_count_drops(&handles), 0);

        for _ in 0..32 {
            assert_matches!(rx.pop(), PopResult::Data(_, _));
        }

        assert_eq!(rx.len_hint(), 32);
        assert_eq!(check_and_count_drops(&handles), 32);

        let dd = DropDummy::new_untracked();

        // big skip in the sequence number, this should "invalidate" the contents of the buffer
        // by setting all CellStates to Skip
        // the 32 remaining tracked dummies will still be stored, not dropped, but not retrievable
        // anymore. i don't consider this to be a memory leak, because the dummies will be dropped
        // when the corresponding index is overwritten again.
        assert!(tx.push(1000, dd).is_ok());

        assert_eq!(rx.len_hint(), 33);

        // dummies still there, not dropped
        assert_eq!(check_and_count_drops(&handles), 32);

        // the untracked dummy with seq 1000 is the next pop result, the reader just skipped over 32
        // cells with CellState::Skip
        assert_matches!(rx.pop(), PopResult::Data(1000, DropDummy::Untracked));

        // still there
        assert_eq!(check_and_count_drops(&handles), 32);

        // observe that we just reduced len_hint from 33 to 0 with a single pop
        assert_eq!(rx.len_hint(), 0);

        // pushing 31 further dummies
        for i in 1001..1032 {
            let dd = DropDummy::new_untracked();
            assert!(tx.push(i, dd).is_ok());
        }

        // the dummies are still there, because the previous 31 untracked ones just overwrote
        // different slots in the buffer
        assert_eq!(check_and_count_drops(&handles), 32);

        // this is getting a bit mean, we should set them free now

        // pushing 32 further dummies
        for i in 1032..1064 {
            let dd = DropDummy::new_untracked();
            assert!(tx.push(i, dd).is_ok());
        }

        assert_eq!(check_and_count_drops(&handles), 64);

        drop(tx);
        drop(rx);

        assert_eq!(check_and_count_drops(&handles), 64)
    }
}

#[cfg(test)]
#[cfg(not(loom))]
mod concurrent_tests {
    use std::{assert_matches, sync::atomic::AtomicBool, thread};

    use rand::{RngExt, SeedableRng};

    use super::*;

    #[test]
    fn basic() {
        let (mut tx, mut rx) = new_pair::<u64, 8>();
        let vals_to_push = (0..100).map(|i| (i, i as u64)).collect::<Vec<(u16, u64)>>();
        let expected = vals_to_push.clone();

        let h = std::thread::spawn(move || {
            for (seq, data) in vals_to_push {
                loop {
                    match tx.push(seq, data) {
                        Ok(()) => break,
                        Err(PushError::Full(s, d)) => {
                            assert_eq!((s, d), (seq, data));
                        }
                        Err(PushError::Stale(_, _)) => unreachable!(),
                    }
                }
            }
        });

        let mut seen = Vec::with_capacity(expected.len());

        while seen.len() < expected.len() {
            loop {
                match rx.pop() {
                    PopResult::Data(s, d) => {
                        seen.push((s, d));
                        break;
                    }
                    PopResult::Missing(_) => unreachable!(),
                    PopResult::Empty => {}
                }
            }
        }

        h.join().unwrap();

        assert_eq!(seen, expected);
        assert_matches!(rx.pop(), PopResult::Empty);
    }

    #[test]
    fn unique() {
        let vals_to_push = (0..100).map(|i| (i, i as u64)).collect::<Vec<(u16, u64)>>();

        for _ in 0..100 {
            let (tx, rx) = new_pair();
            test_unique_helper(vals_to_push.clone(), tx, rx);
        }
    }

    fn test_unique_helper(
        vals_to_push: Vec<(u16, u64)>,
        mut tx: Writer<u64, 8>,
        mut rx: Reader<u64, 8>,
    ) {
        let expected = vals_to_push.clone();

        assert_eq!(expected.len(), vals_to_push.len());

        let should_exit = Arc::new(AtomicBool::new(false));

        let should_exit_cl = should_exit.clone();

        let h = std::thread::spawn(move || {
            for (seq, data) in vals_to_push {
                loop {
                    if should_exit_cl.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    match tx.push(seq, data) {
                        Ok(()) => break,
                        Err(PushError::Full(s, d)) => {
                            thread::yield_now();
                            assert_eq!((s, d), (seq, data));
                        }
                        Err(PushError::Stale(s, d)) => {
                            assert_eq!((s, d), (seq, data));
                            break;
                        }
                    }
                }
            }
        });

        let mut seen = Vec::with_capacity(expected.len());

        'outer: while seen.len() <= expected.len() {
            loop {
                match rx.pop() {
                    PopResult::Empty => {
                        if h.is_finished() {
                            break 'outer;
                        }
                    }
                    res => {
                        seen.push(res);
                        break;
                    }
                }
            }
        }

        should_exit.store(true, std::sync::atomic::Ordering::Relaxed);
        h.join().unwrap();

        for (s, d) in seen.into_iter().enumerate() {
            match d {
                PopResult::Missing(seq) => assert_eq!(seq, s as u16),
                PopResult::Data(seq, u) => {
                    assert_eq!(seq, s as u16);
                    assert_eq!(u, s as u64);
                }
                PopResult::Empty => unreachable!(),
            }
        }
    }

    #[test]
    fn unique_with_reorder() {
        fn permute_slightly(v: &mut [(u16, u64)]) {
            const MAX_REORDER_DIST: usize = 2; // WARN: should really depend on buffer size
            let mut already_reordered = vec![false; v.len()];
            let mut rng = rand::rngs::SmallRng::seed_from_u64(123);

            for i in 1..v.len() {
                let j = rng.random_range((i + 1)..=(i + MAX_REORDER_DIST));
                if j < v.len() && !already_reordered[j] && !already_reordered[i] {
                    already_reordered[j] = true;
                    already_reordered[i] = true;
                    (v[i], v[j]) = (v[j], v[i]);
                }
            }
        }
        let mut vals_to_push = (0..100).map(|i| (i, i as u64)).collect::<Vec<(u16, u64)>>();
        permute_slightly(&mut vals_to_push);

        let mut max_gap = 0;

        for i in 1..vals_to_push.len() {
            let gap = vals_to_push[i].0.abs_diff(vals_to_push[i - 1].0);
            max_gap = max_gap.max(gap);
        }

        assert!(max_gap < 8);

        for _ in 0..100 {
            let (tx, rx) = new_pair();
            test_unique_helper(vals_to_push.clone(), tx, rx);
        }
    }
}

#[cfg(test)]
#[cfg(loom)]
mod loom_tests {
    use super::*;
    use std::assert_matches;

    #[test]
    fn basic() {
        loom::model(|| {
            let (mut tx, mut rx) = new_pair::<u64, 4>();

            let vals_to_push = vec![(0, 0), (1, 1)];

            let expected = vals_to_push.clone();

            let h = loom::thread::spawn(move || {
                for (seq, data) in vals_to_push {
                    assert!(tx.push(seq, data).is_ok())
                }
            });

            let mut seen = Vec::new();
            while seen.len() < expected.len() {
                match rx.pop() {
                    PopResult::Empty => loom::thread::yield_now(),
                    PopResult::Data(seq, data) => seen.push((seq, data)),
                    _ => unreachable!(),
                }
            }

            assert_matches!(rx.pop(), PopResult::Empty);

            assert_eq!(seen, expected);
            h.join().unwrap();
        });
    }

    #[test]
    fn reorder_insert() {
        loom::model(|| {
            let (mut tx, mut rx) = new_pair::<u64, 4>();

            let vals_to_push = vec![(0, 0), (2, 2), (1, 1)];

            let expected = vals_to_push.clone();

            let h = loom::thread::spawn(move || {
                assert_matches!(tx.push(0, 0), Ok(()));
                assert_matches!(tx.push(2, 2), Ok(()));
                // if the reader already read 0, we can't insert 1 anymore
                assert_matches!(tx.push(1, 1), Err(PushError::Stale(1, 1)) | Ok(()));
            });

            let mut seen = Vec::with_capacity(expected.len());
            while seen.len() < expected.len() {
                match rx.pop() {
                    PopResult::Empty => loom::thread::yield_now(),
                    s => seen.push(s),
                }
            }

            assert_matches!(seen[0], PopResult::Data(0, 0u64));
            assert_matches!(seen[1], PopResult::Data(1, 1) | PopResult::Missing(1));
            assert_matches!(seen[2], PopResult::Data(2, 2));
            h.join().unwrap();
        });
    }
}
