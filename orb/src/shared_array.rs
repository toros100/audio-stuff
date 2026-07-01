use std::marker::PhantomData;
use std::mem::MaybeUninit;

#[cfg(loom)]
use loom::cell::UnsafeCell;

#[cfg(not(loom))]
use std::cell::UnsafeCell;

fn with_cell<T, R>(cell: &UnsafeCell<T>, f: impl FnOnce(*const T) -> R) -> R {
    #[cfg(loom)]
    return cell.with(f);

    #[cfg(not(loom))]
    return f(cell.get());
}

fn with_cell_mut<T, R>(cell: &UnsafeCell<T>, f: impl FnOnce(*mut T) -> R) -> R {
    #[cfg(loom)]
    return cell.with_mut(f);

    #[cfg(not(loom))]
    return f(cell.get());
}

pub(crate) struct SharedArray<T, const N: usize> {
    vals: [UnsafeCell<MaybeUninit<T>>; N],
    live_vals: [UnsafeCell<bool>; N],
    _phantom: PhantomData<T>,
}

impl<T, const N: usize> Default for SharedArray<T, N> {
    fn default() -> Self {
        Self {
            vals: std::array::from_fn(|_| UnsafeCell::new(MaybeUninit::uninit())),
            live_vals: std::array::from_fn(|_| UnsafeCell::new(false)),
            _phantom: PhantomData,
        }
    }
}

impl<T, const N: usize> SharedArray<T, N> {
    /// # Safety
    /// takes ownership of the value at index i < N, which must actually contain a value prior to
    /// calling this.
    /// may not be called concurrently with any method on SharedArray on the same i.
    pub(crate) unsafe fn take(&self, i: usize) -> T {
        unsafe {
            with_cell_mut(&self.live_vals[i], |p| {
                debug_assert!(*p, "value should be live when taking");
                *p = false;
                with_cell(&self.vals[i], |q| (*q).assume_init_read())
            })
        }
    }

    /// # Safety
    /// inserts a value at index i < N. the previous value, if any, is dropped.
    /// may not be called concurrently with any method on SharedArray on the same i.
    pub(crate) unsafe fn insert(&self, i: usize, t: T) {
        unsafe {
            with_cell_mut(&self.live_vals[i], |p| {
                with_cell_mut(&self.vals[i], |q| {
                    if *p {
                        (*q).assume_init_drop();
                        (*q).write(t);
                    } else {
                        (*q).write(t);
                        (*p) = true;
                    }
                })
            });
        }
    }

    /// drops the value at index i < N, if there is one.
    /// may not be called concurrently with any method on SharedArray on the same i.
    pub(crate) unsafe fn drop_if_present(&self, i: usize) {
        unsafe {
            with_cell_mut(&self.live_vals[i], |p| {
                if *p {
                    with_cell_mut(&self.vals[i], |q| (*q).assume_init_drop());
                    *p = false;
                }
            })
        };
    }
}
