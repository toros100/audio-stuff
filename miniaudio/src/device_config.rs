use std::marker::PhantomData;

use crate::ma_bindings::ma_device_config;
use crate::{ApplyConfig, DeviceId, DirCapture, DirPlayback, SampleFormat};

// idea: config types specific to the device type (playback/capture/duplex) with nice fluent setter
// style methods. internally, all fields are Option<_>, and via the ApplyConfig trait, anything that
// was explicitly configured is applied to an actual ma_device_config.
// the initial ma_device_config is provided by miniaudio itself (ma_device_config_init), which is
// supposed to make it possible to add fields without breaking existing code according to the
// miniaudio docs. this property is preserved here: taking the default value of
// PlaybackDeviceConfig/CaptureDeviceConfig/DuplexDeviceConfig will essentially result in the
// corresponding miniaudio provided default ma_device_config being used (aside from the
// sample format configuration, which is represented as a generic here, because it is required to
// properly type the callback)

// TODO: need a way to inspect the actual/negotiated config on a device
// this is especially important when not configuring certain values explicitly (such as sample_rate
// or num_channels, which will let it use the devices native config)
// (also, this is clearly not complete in any way)

#[derive(Default)]
pub struct PlaybackDeviceConfig<PlaybackFormat: SampleFormat> {
    general: GeneralOptions,
    playback: DirectionalOptions<PlaybackFormat, DirPlayback>,
}

impl<P: SampleFormat> PlaybackDeviceConfig<P> {
    pub fn general(&mut self) -> &mut GeneralOptions {
        &mut self.general
    }
    pub fn playback(&mut self) -> &mut DirectionalOptions<P, DirPlayback> {
        &mut self.playback
    }
}

#[derive(Default)]
pub struct CaptureDeviceConfig<CaptureFormat: SampleFormat> {
    general: GeneralOptions,
    capture: DirectionalOptions<CaptureFormat, DirCapture>,
}
impl<C: SampleFormat> CaptureDeviceConfig<C> {
    pub fn general(&mut self) -> &mut GeneralOptions {
        &mut self.general
    }
    pub fn capture(&mut self) -> &mut DirectionalOptions<C, DirCapture> {
        &mut self.capture
    }
}

#[derive(Default)]
pub struct DuplexDeviceConfig<PlaybackFormat: SampleFormat, CaptureFormat: SampleFormat> {
    general: GeneralOptions,
    capture: DirectionalOptions<CaptureFormat, DirCapture>,
    playback: DirectionalOptions<PlaybackFormat, DirPlayback>,
}

impl<P, C> DuplexDeviceConfig<P, C>
where
    P: SampleFormat,
    C: SampleFormat,
{
    pub fn general(&mut self) -> &mut GeneralOptions {
        &mut self.general
    }
    pub fn playback(&mut self) -> &mut DirectionalOptions<P, DirPlayback> {
        &mut self.playback
    }
    pub fn capture(&mut self) -> &mut DirectionalOptions<C, DirCapture> {
        &mut self.capture
    }
}

#[derive(Default)]
/// options shared between all device types
pub struct GeneralOptions {
    sample_rate: Option<u32>,
    frame_length: Option<u32>,
}

impl GeneralOptions {
    pub fn sample_rate(&mut self, v: u32) -> &mut Self {
        self.sample_rate = Some(v);
        self
    }

    pub fn frame_length(&mut self, v: u32) -> &mut Self {
        self.frame_length = Some(v);
        self
    }
}

/// options specific to a playback or capture device
pub struct DirectionalOptions<F: SampleFormat, R> {
    _phantom: PhantomData<(F, R)>,
    channel_count: Option<u32>,
    device_id: Option<DeviceId<R>>,
}

impl<F: SampleFormat, D> DirectionalOptions<F, D> {
    pub fn channel_count(&mut self, v: u32) -> &mut Self {
        self.channel_count = Some(v);
        self
    }
    pub fn device_id(&mut self, v: DeviceId<D>) -> &mut Self {
        self.device_id = Some(v);
        self
    }
}

impl<F: SampleFormat> Default for DirectionalOptions<F, DirCapture> {
    fn default() -> Self {
        Self {
            _phantom: PhantomData,
            channel_count: None,
            device_id: None,
        }
    }
}

impl<F: SampleFormat> Default for DirectionalOptions<F, DirPlayback> {
    fn default() -> Self {
        Self {
            _phantom: PhantomData,
            channel_count: None,
            device_id: None,
        }
    }
}

unsafe impl<F: SampleFormat> ApplyConfig for DirectionalOptions<F, DirPlayback> {
    fn apply(&self, cfg: &mut ma_device_config) {
        cfg.playback.format = F::MA_FORMAT;
        if let Some(v) = self.channel_count {
            cfg.playback.channels = v
        }
        if let Some(id) = self.device_id.as_ref() {
            cfg.playback.pDeviceID = &id.0
        }
    }
}

unsafe impl ApplyConfig for GeneralOptions {
    fn apply(&self, cfg: &mut ma_device_config) {
        if let Some(v) = self.sample_rate {
            cfg.sampleRate = v;
        }
        if let Some(v) = self.frame_length {
            cfg.periodSizeInFrames = v;
        }
    }
}

unsafe impl<F: SampleFormat> ApplyConfig for DirectionalOptions<F, DirCapture> {
    fn apply(&self, cfg: &mut ma_device_config) {
        cfg.capture.format = F::MA_FORMAT;
        if let Some(v) = self.channel_count {
            cfg.capture.channels = v
        }
        if let Some(id) = self.device_id.as_ref() {
            cfg.capture.pDeviceID = &id.0
        }
    }
}

unsafe impl<P: SampleFormat> ApplyConfig for PlaybackDeviceConfig<P> {
    fn apply(&self, cfg: &mut ma_device_config) {
        self.playback.apply(cfg);
        self.general.apply(cfg);
    }
}

unsafe impl<C: SampleFormat> ApplyConfig for CaptureDeviceConfig<C> {
    fn apply(&self, cfg: &mut ma_device_config) {
        self.capture.apply(cfg);
        self.general.apply(cfg);
    }
}

unsafe impl<P: SampleFormat, C: SampleFormat> ApplyConfig for DuplexDeviceConfig<P, C> {
    fn apply(&self, cfg: &mut ma_device_config) {
        self.general.apply(cfg);
        self.playback.apply(cfg);
        self.capture.apply(cfg);
    }
}
