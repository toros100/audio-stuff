mod device;
mod ma_bindings;
pub use device::*;
pub use device_config::*;
mod context;
pub use context::*;
mod device_config;
// mod filter;

pub(crate) trait MaResultExt {
    fn into_result(self) -> Result<(), MaError>;
}

impl MaResultExt for ma_bindings::ma_result {
    fn into_result(self) -> Result<(), MaError> {
        match self {
            #[allow(non_upper_case_globals)]
            ma_bindings::ma_result_MA_SUCCESS => Ok(()),
            r => Err(MaError(r)),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("miniaudio error {}: {}",self.0, self.get_description())]
pub struct MaError(ma_bindings::ma_result);

impl MaError {
    fn get_description(&self) -> &'static str {
        unsafe {
            let p = ma_bindings::ma_result_description(self.0);
            std::ffi::CStr::from_ptr(p).to_str().unwrap_or("unknown")
        }
    }
    pub fn code(&self) -> i32 {
        self.0
    }
}

/// # Safety
/// TODO
pub unsafe trait SampleFormat: 'static + private::Sealed + Default + Send + Copy {
    const MA_FORMAT: ma_bindings::ma_format;
}

unsafe impl SampleFormat for f32 {
    const MA_FORMAT: ma_bindings::ma_format = ma_bindings::ma_format_ma_format_f32;
}
unsafe impl SampleFormat for i16 {
    const MA_FORMAT: ma_bindings::ma_format = ma_bindings::ma_format_ma_format_s16;
}
unsafe impl SampleFormat for i32 {
    const MA_FORMAT: ma_bindings::ma_format = ma_bindings::ma_format_ma_format_s32;
}
unsafe impl SampleFormat for u8 {
    const MA_FORMAT: ma_bindings::ma_format = ma_bindings::ma_format_ma_format_u8;
}

pub(crate) mod private {
    use crate::SampleFormat;
    use crate::device::{Capture, Duplex, Playback};
    // cf. https://rust-lang.github.io/api-guidelines/future-proofing.html
    pub trait Sealed {}

    impl Sealed for f32 {}
    impl Sealed for i16 {}
    impl Sealed for i32 {}
    impl Sealed for u8 {}

    impl<F, CB> Sealed for Playback<F, CB>
    where
        F: SampleFormat,
        CB: FnMut(&mut [F]) + Send + 'static,
    {
    }

    impl<F, CB> Sealed for Capture<F, CB>
    where
        F: SampleFormat,
        CB: FnMut(&[F]) + Send + 'static,
    {
    }

    impl<P, C, CB> Sealed for Duplex<P, C, CB>
    where
        P: SampleFormat,
        C: SampleFormat,
        CB: FnMut(&mut [P], &[C]) + Send + 'static,
    {
    }
}
