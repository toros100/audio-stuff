use std::marker::PhantomData;
use std::os::raw::c_void;
use std::ptr::null;

use crate::ma_bindings::{
    ma_format_ma_format_f32, ma_format_ma_format_s16, ma_hpf, ma_hpf_config_init, ma_hpf_init,
    ma_hpf_process_pcm_frames, ma_hpf_uninit,
};

use crate::{MaError, MaResultExt, SampleFormat};

pub struct HighPassFilter<F> {
    inner: Box<ma_hpf>,
    _phantom: PhantomData<F>,
}

// ma docs: "Supported formats are ma_format_s16 and ma_format_f32. If you need to use a different
// format you need to convert it yourself beforehand. Input and output frames are always interleaved."
const fn acceptable_hpf_format<F: SampleFormat>() -> bool {
    matches!(
        F::MA_FORMAT,
        ma_format_ma_format_s16 | ma_format_ma_format_f32
    )
}

// TODO: configure channel count
impl<F: SampleFormat> HighPassFilter<F> {
    pub fn try_new(freq: f64, order: u32, sample_rate: u32) -> Result<Self, MaError> {
        const {
            assert!(acceptable_hpf_format::<F>());
        }

        // TODO: order is limited to ma_max_filter_order
        // should check what miniaudio does in that case (error or clamp?)

        let cfg =
            unsafe { ma_hpf_config_init(ma_format_ma_format_f32, 1, sample_rate, freq, order) };

        let mut inner: Box<ma_hpf> = unsafe { Box::new(std::mem::zeroed()) };

        unsafe { ma_hpf_init(&cfg, null(), inner.as_mut()) }.into_result()?;
        Ok(Self {
            inner,
            _phantom: PhantomData,
        })
    }

    pub fn process(&mut self, input: &mut [f32]) -> Result<(), MaError> {
        // NOTE: hardcoded mono assumption for now
        unsafe {
            ma_hpf_process_pcm_frames(
                self.inner.as_mut(),
                input as *mut _ as *mut c_void,
                input as *mut _ as *mut c_void,
                input.len() as u64,
            )
        }
        .into_result()
    }
}

impl<F> Drop for HighPassFilter<F> {
    fn drop(&mut self) {
        unsafe {
            ma_hpf_uninit(self.inner.as_mut(), null());
        }
    }
}
