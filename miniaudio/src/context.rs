use std::sync::Arc;

use crate::{
    Capture, CaptureDeviceConfig, Device, DeviceId, DirCapture, DirPlayback, Duplex,
    DuplexDeviceConfig, MaError, MaResultExt, Playback, PlaybackDeviceConfig, SampleFormat,
};

use crate::ma_bindings::{
    ma_context, ma_context_get_devices, ma_context_init, ma_context_uninit, ma_device_info,
    ma_uint32,
};

#[derive(Debug)]
pub struct DeviceInfo<Dir> {
    pub name: String,
    pub is_default: bool,
    pub device_id: DeviceId<Dir>,
}

pub struct DeviceList {
    pub playback_devices: Vec<DeviceInfo<DirPlayback>>,
    pub capture_devices: Vec<DeviceInfo<DirCapture>>,
}

// TODO: context config?
pub struct Context {
    // Arc instead of box because devices spawned from this context will
    // hold on to a clone of this Arc, to ensure the Context stays alive
    // for the lifetime of the device
    inner: Arc<ContextInner>,
}

#[repr(transparent)] // important
pub(crate) struct ContextInner(ma_context);

impl Drop for ContextInner {
    fn drop(&mut self) {
        unsafe {
            ma_context_uninit(&mut self.0);
        }
    }
}

impl Context {
    pub(crate) fn inner_ptr(&self) -> *mut ma_context {
        Arc::as_ptr(&self.inner) as *mut ma_context
    }

    pub(crate) fn inner_clone(&self) -> Arc<ContextInner> {
        self.inner.clone()
    }

    pub fn new() -> Result<Self, MaError> {
        #[allow(clippy::arc_with_non_send_sync)]
        let inner = Arc::new(ContextInner(unsafe { std::mem::zeroed::<ma_context>() }));

        let inner_ptr = Arc::as_ptr(&inner) as *mut ma_context;

        unsafe { ma_context_init(std::ptr::null(), 0, std::ptr::null(), inner_ptr) }
            .into_result()?;

        Ok(Self { inner })
    }

    pub fn get_devices(&mut self) -> Result<DeviceList, MaError> {
        let inner_ptr = Arc::as_ptr(&self.inner) as *mut ma_context;
        let mut playback_device_count: ma_uint32 = 0;
        let mut capture_device_count: ma_uint32 = 0;

        let mut playback_device_info_ptr: *mut ma_device_info = std::ptr::null_mut();
        let mut capture_device_info_ptr: *mut ma_device_info = std::ptr::null_mut();

        unsafe {
            ma_context_get_devices(
                inner_ptr,
                &mut playback_device_info_ptr,
                &mut playback_device_count,
                &mut capture_device_info_ptr,
                &mut capture_device_count,
            )
        }
        .into_result()?;

        let playback_device_info = unsafe {
            std::slice::from_raw_parts::<ma_device_info>(
                playback_device_info_ptr,
                playback_device_count as usize,
            )
        };
        let capture_device_info = unsafe {
            std::slice::from_raw_parts::<ma_device_info>(
                capture_device_info_ptr,
                capture_device_count as usize,
            )
        };

        let mut l = DeviceList {
            playback_devices: Vec::with_capacity(playback_device_info.len()),
            capture_devices: Vec::with_capacity(capture_device_info.len()),
        };

        for d in playback_device_info.iter() {
            let name = unsafe { std::ffi::CStr::from_ptr(d.name.as_ptr()) }
                .to_string_lossy()
                .into_owned();

            let device_id = DeviceId::<DirPlayback>::from(d.id);
            let is_default = d.isDefault == 1;
            l.playback_devices.push(DeviceInfo {
                name,
                device_id,
                is_default,
            });
        }

        for d in capture_device_info.iter() {
            let name = unsafe { std::ffi::CStr::from_ptr(d.name.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            let device_id = DeviceId::<DirCapture>::from(d.id);
            let is_default = d.isDefault == 1;
            l.capture_devices.push(DeviceInfo {
                name,
                device_id,
                is_default,
            });
        }

        Ok(l)
    }

    pub fn new_playback_device<P, CB>(
        &mut self,
        callback: CB,
        opt: PlaybackDeviceConfig<P>,
    ) -> Result<Device<Playback<P, CB>>, MaError>
    where
        P: SampleFormat,
        CB: FnMut(&mut [P]) + Send + 'static,
    {
        Device::new(callback, opt, self)
    }

    pub fn new_capture_device<C, CB>(
        &mut self,
        callback: CB,
        opt: CaptureDeviceConfig<C>,
    ) -> Result<Device<Capture<C, CB>>, MaError>
    where
        C: SampleFormat,
        CB: FnMut(&[C]) + Send + 'static,
    {
        Device::new(callback, opt, self)
    }

    pub fn new_duplex_device<P, C, CB>(
        &mut self,
        callback: CB,
        opt: DuplexDeviceConfig<P, C>,
    ) -> Result<Device<Duplex<P, C, CB>>, MaError>
    where
        P: SampleFormat,
        C: SampleFormat,
        CB: FnMut(&mut [P], &[C]) + Send + 'static,
    {
        Device::new(callback, opt, self)
    }
}
