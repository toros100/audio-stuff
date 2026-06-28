#![allow(non_upper_case_globals)]

use std::ffi::c_void;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::sync::Arc;

use crate::ma_bindings::{
    ma_device, ma_device_config, ma_device_config_init, ma_device_id, ma_device_init,
    ma_device_start, ma_device_stop, ma_device_type, ma_device_type_ma_device_type_capture,
    ma_device_type_ma_device_type_duplex, ma_device_type_ma_device_type_playback, ma_device_uninit,
    ma_uint32,
};

use crate::device_config::{CaptureDeviceConfig, DuplexDeviceConfig, PlaybackDeviceConfig};
use crate::{Context, ContextInner, MaError, MaResultExt, SampleFormat};

type GlueCallback = unsafe extern "C" fn(*mut ma_device, *mut c_void, *const c_void, u32);

/// # Safety
/// (for internal documentation, external users will not implemement this trait)
pub unsafe trait DeviceSpec: crate::private::Sealed {
    /// used to initialize the miniaudio internal default device config
    const MA_DEVICE_TYPE: ma_device_type;
    /// this callback is what is actually registered with the miniaudio device. it is responsible
    /// for doing unsafe conversions from C types into the actual input types of the Self::Callback closure,
    /// which is stored in and retrieved from the devices user data, and then calling that closure.
    const GLUE_CALLBACK: GlueCallback;
    /// type used to configure a miniaudio device.
    type Options: ApplyConfig;
    /// the user-defined callback that will process properly typed sample data. this approach allows
    /// for a properly typed Callback without dyn or user-facing unsafe.
    type Callback: Send + 'static;
}

/// # Safety
/// the callback should be considered to be mutably borrowed for the lifetime of the device, in
/// particular it needs to be kept alive.
unsafe fn register_callback<D: DeviceSpec>(cfg: &mut ma_device_config, callback: &mut D::Callback) {
    cfg.dataCallback = Some(D::GLUE_CALLBACK);
    cfg.pUserData = callback as *mut _ as *mut c_void;
}

#[derive(Clone, Copy)]
pub struct DeviceId<Dir>(pub(crate) ma_device_id, PhantomData<Dir>);

// not implementing the From trait because this should not be public
impl DeviceId<DirPlayback> {
    pub(crate) fn from(v: ma_device_id) -> Self {
        Self(v, PhantomData)
    }
}
impl DeviceId<DirCapture> {
    pub(crate) fn from(v: ma_device_id) -> Self {
        Self(v, PhantomData)
    }
}

impl<Dir> Debug for DeviceId<Dir> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // can't really do better here, it's a c union with who knows what inside
        // (depends on the backend)
        write!(f, "[DeviceId]")
    }
}

/// # Safety
/// used internally to generically apply a (partial) device configuration to the actual
/// ma_device_config. should not be implemented by external users.
pub unsafe trait ApplyConfig {
    fn apply(&self, cfg: &mut ma_device_config);
}

#[derive(Debug)]
pub struct DirPlayback;

#[derive(Debug)]
pub struct DirCapture;

pub struct Playback<P: SampleFormat, CB: FnMut(&mut [P]) + Send + 'static> {
    _phantom: PhantomData<(P, CB)>,
}

pub struct Capture<C: SampleFormat, CB: FnMut(&[C]) + Send + 'static> {
    _phantom: PhantomData<(C, CB)>,
}

pub struct Duplex<P: SampleFormat, C: SampleFormat, CB: FnMut(&mut [P], &[C]) + Send + 'static> {
    _phantom: PhantomData<(P, C, CB)>,
}

unsafe impl<F, CB> DeviceSpec for Playback<F, CB>
where
    F: SampleFormat,
    CB: FnMut(&mut [F]) + Send + 'static,
{
    const MA_DEVICE_TYPE: ma_device_type = ma_device_type_ma_device_type_playback;
    const GLUE_CALLBACK: GlueCallback = c_playback_callback::<F, CB>;
    type Callback = CB;
    type Options = PlaybackDeviceConfig<F>;
}

unsafe impl<F, CB> DeviceSpec for Capture<F, CB>
where
    F: SampleFormat,
    CB: FnMut(&[F]) + Send + 'static,
{
    const MA_DEVICE_TYPE: ma_device_type = ma_device_type_ma_device_type_capture;
    const GLUE_CALLBACK: GlueCallback = c_capture_callback::<F, CB>;
    type Callback = CB;
    type Options = CaptureDeviceConfig<F>;
}

unsafe impl<P, C, CB> DeviceSpec for Duplex<P, C, CB>
where
    P: SampleFormat,
    C: SampleFormat,
    CB: FnMut(&mut [P], &[C]) + Send + 'static,
{
    const MA_DEVICE_TYPE: ma_device_type = ma_device_type_ma_device_type_duplex;
    const GLUE_CALLBACK: GlueCallback = c_duplex_callback::<P, C, CB>;
    type Callback = CB;
    type Options = DuplexDeviceConfig<P, C>;
}

pub struct Device<M: DeviceSpec> {
    inner: Box<ma_device>,
    // to ensure the context is kept alive for the lifetime of the device
    _ctx: Arc<ContextInner>,
    _boxed_callback: Box<M::Callback>,
    _phantom: PhantomData<M>,
}

impl<M: DeviceSpec> Device<M> {
    pub fn start(&mut self) -> Result<(), MaError> {
        unsafe { ma_device_start(self.inner.as_mut()) }.into_result()
    }

    pub fn stop(&mut self) -> Result<(), MaError> {
        unsafe { ma_device_stop(self.inner.as_mut()) }.into_result()
    }

    pub(crate) fn new(
        callback: M::Callback,
        opt: M::Options,
        ctx: &mut Context,
    ) -> Result<Device<M>, MaError> {
        // ma docs: "The config object can be allocated on the stack and does not need
        // to be maintained after initialization of the corresponding object. "

        let mut cfg: ma_device_config = unsafe { ma_device_config_init(M::MA_DEVICE_TYPE) };
        opt.apply(&mut cfg);

        let mut boxed_callback = Box::new(callback);

        // SAFETY: we own the callback and box it, ensuring exclusive access and keeping it alive
        // for the lifetime of the device
        unsafe {
            register_callback::<M>(&mut cfg, &mut boxed_callback);
        }

        let mut inner = Box::new(unsafe { std::mem::zeroed::<ma_device>() });
        let inner_ptr = inner.as_mut();
        let ctx_ptr = ctx.inner_ptr();

        unsafe { ma_device_init(ctx_ptr, &cfg, inner_ptr) }.into_result()?;

        Ok(Device::<M> {
            _ctx: ctx.inner_clone(),
            inner,
            _boxed_callback: boxed_callback,
            _phantom: PhantomData,
        })
    }
}

impl<M: DeviceSpec> Drop for Device<M> {
    fn drop(&mut self) {
        unsafe {
            ma_device_uninit(self.inner.as_mut());
        }
    }
}

extern "C" fn c_playback_callback<P, CB>(
    device: *mut ma_device,
    playback_data: *mut c_void,
    _: *const c_void,
    frame_count: ma_uint32,
) where
    P: SampleFormat,
    CB: FnMut(&mut [P]) + Send + 'static,
{
    unsafe {
        debug_assert_eq!((*device).playback.format, P::MA_FORMAT);

        let p_len = frame_count as usize * (*device).playback.channels as usize;

        let playback_samples = std::slice::from_raw_parts_mut(playback_data as *mut P, p_len);

        let callback = &mut *((*device).pUserData as *mut CB);
        callback(playback_samples);
    }
}

extern "C" fn c_capture_callback<C, CB>(
    device: *mut ma_device,
    _: *mut c_void,
    captured_data: *const c_void,
    frame_count: ma_uint32,
) where
    C: SampleFormat,
    CB: FnMut(&[C]) + Send + 'static,
{
    unsafe {
        debug_assert_eq!((*device).capture.format, C::MA_FORMAT);

        let c_len = frame_count as usize * (*device).capture.channels as usize;

        let captured_samples = std::slice::from_raw_parts(captured_data as *const C, c_len);

        let callback = &mut *((*device).pUserData as *mut CB);
        callback(captured_samples);
    }
}

extern "C" fn c_duplex_callback<P, C, CB>(
    device: *mut ma_device,
    playback_data: *mut c_void,
    captured_data: *const c_void,
    frame_count: ma_uint32,
) where
    P: SampleFormat,
    C: SampleFormat,
    CB: FnMut(&mut [P], &[C]) + Send + 'static,
{
    unsafe {
        debug_assert_eq!((*device).playback.format, P::MA_FORMAT);
        debug_assert_eq!((*device).capture.format, C::MA_FORMAT);

        let c_len = frame_count as usize * (*device).capture.channels as usize;
        let p_len = frame_count as usize * (*device).playback.channels as usize;

        let captured_samples = std::slice::from_raw_parts(captured_data as *const C, c_len);
        let playback_samples = std::slice::from_raw_parts_mut(playback_data as *mut P, p_len);

        let callback = &mut *((*device).pUserData as *mut CB);
        callback(playback_samples, captured_samples);
    }
}
