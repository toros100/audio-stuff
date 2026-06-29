use std::{cell::UnsafeCell, mem::MaybeUninit};

pub(crate) struct SharedArray<T, const N: usize> {
    vals: [UnsafeCell<MaybeUninit<T>>; N],
    live_vals: [UnsafeCell<bool>; N],
}

impl<T, const N: usize> SharedArray<T, N> {
    /// # Safety
    /// takes ownership of the value at index i < N, which must actually contain a value prior to
    /// calling this.
    /// may not be called concurrently with any method on SharedArray on the same i.
    pub(crate) unsafe fn take(&self, i: usize) -> T {
        if cfg!(debug_assertions) {
            let live = unsafe { *self.live_vals[i].get() };
            debug_assert!(live);
        }

        unsafe {
            self.live_vals[i].get().write(false);
            (*self.vals[i].get()).assume_init_read()
        }
    }

    /// # Safety
    /// inserts a value at index i < N. the previous value, if any, is dropped.
    /// may not be called concurrently with any method on SharedArray on the same i.
    pub(crate) unsafe fn insert(&self, i: usize, t: T) {
        unsafe {
            let live = *self.live_vals[i].get();

            if live {
                (*self.vals[i].get()).assume_init_drop();
            } else {
                self.live_vals[i].get().write(true);
            }

            (*self.vals[i].get()).write(t);
        }
    }

    /// drops the value at index i < N, if there is one.
    /// may not be called concurrently with any method on SharedArray on the same i.
    pub(crate) unsafe fn drop_if_present(&self, i: usize) {
        unsafe {
            let live = *self.live_vals[i].get();
            if live {
                (*self.vals[i].get()).assume_init_drop();
                self.live_vals[i].get().write(false);
            }
        };
    }
}

impl<T, const N: usize> Default for SharedArray<T, N> {
    fn default() -> Self {
        let vals = std::array::from_fn(|_| UnsafeCell::new(MaybeUninit::uninit()));
        let live_vals = std::array::from_fn(|_| UnsafeCell::new(false));
        Self { vals, live_vals }
    }
}
impl<T, const N: usize> Drop for SharedArray<T, N> {
    fn drop(&mut self) {
        for i in 0..N {
            unsafe {
                self.drop_if_present(i);
            }
        }
    }
}
