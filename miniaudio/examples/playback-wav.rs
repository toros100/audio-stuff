use std::path::PathBuf;

use clap::Parser;
#[derive(clap::Parser)]
struct Args {
    /// path to .wav file that will be played back on the default playback device
    #[arg(short, long)]
    file: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let name = args
        .file
        .file_name()
        .ok_or(anyhow::anyhow!("unexpected input (no file name)"))?
        .to_owned();

    // loading file content into Vec to avoid doing file io in the audio callback
    let input = std::fs::read(args.file)?;
    let reader = hound::WavReader::new(std::io::Cursor::new(input))?;

    let spec = reader.spec();
    let sample_rate = spec.sample_rate;
    let channels = spec.channels as u32;

    match spec.sample_format {
        hound::SampleFormat::Int => play::<i32>(sample_rate, channels, name, reader.into_samples()),
        hound::SampleFormat::Float => {
            play::<f32>(sample_rate, channels, name, reader.into_samples())
        }
    }
}

fn play<T: miniaudio::SampleFormat>(
    sample_rate: u32,
    channels: u32,
    name: std::ffi::OsString,
    mut samples_iter: impl Iterator<Item = Result<T, hound::Error>> + Send + 'static,
) -> anyhow::Result<()> {
    let mut cfg = miniaudio::PlaybackDeviceConfig::<T>::default();
    cfg.general().sample_rate(sample_rate);
    cfg.playback().channel_count(channels);

    let mut ctx = miniaudio::Context::new()?;

    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel::<Result<(), hound::Error>>();
    let mut shutdown = Some(shutdown_tx);

    let mut playback_device = ctx.new_playback_device(
        move |f| {
            if shutdown.is_some() {
                for s in f.iter_mut() {
                    match samples_iter.next() {
                        Some(res) => match res {
                            Ok(v) => *s = v,
                            Err(e) => {
                                if let Some(tx) = shutdown.take() {
                                    _ = tx.send(Err(e));
                                }
                            }
                        },
                        None => {
                            if let Some(tx) = shutdown.take() {
                                _ = tx.send(Ok(()))
                            }
                        }
                    }
                }
            }
        },
        cfg,
    )?;

    playback_device.start()?;
    println!("playing file {:?}", name);
    match shutdown_rx.recv() {
        // channel disconnected, audio thread must have died?
        Err(_) => Err(anyhow::anyhow!("unexpected error")),
        Ok(Err(e)) => Err(anyhow::anyhow!("while reading samples: {}", e)),
        _ => Ok(()),
    }
}
