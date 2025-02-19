use std::{
    collections::VecDeque,
    sync::{
        Arc,
        mpsc::SyncSender,
    },
    time::Duration,
};

use atomig::{
    Atom,
    Atomic,
};
use cpal::traits::{
    DeviceTrait,
    HostTrait,
    StreamTrait,
};
use eframe::NativeOptions;
use symphonia::core::{
    audio::AudioBuffer,
    codecs::audio::{
        AudioCodecParameters,
        AudioDecoder,
        AudioDecoderOptions,
    },
    formats::{
        FormatOptions,
        FormatReader,
        probe::Hint,
    },
    io::{
        MediaSourceStream,
        MediaSourceStreamOptions,
    },
    meta::MetadataOptions,
    units::{
        Time,
        TimeBase,
        TimeStamp,
    },
};
use tracing::{
    debug,
    info,
    trace,
};
use tracing_subscriber::{
    Layer,
    filter::Targets,
    fmt,
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let stdout_log = fmt::layer();

    tracing_subscriber::registry()
        .with(
            stdout_log.with_filter(
                Targets::default()
                    .with_target("symphonia", tracing::Level::TRACE)
                    .with_target("musicthing", tracing::Level::TRACE)
                    .with_target("wgpu", tracing::Level::WARN)
                    .with_target("egui", tracing::Level::WARN)
                    .with_target("eframe", tracing::Level::WARN)
                    .with_default(tracing::Level::INFO),
            ),
        )
        .init();

    eframe::run_native(
        "musicthing",
        NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            window_builder: Some(Box::new(|builder| {
                builder.with_title("musicthing").with_app_id("floating") // for me, will fix later - GLS
            })),
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )?;
    Ok(())
}

/// # Panics
/// FIXME: Error Handling
pub fn empty_stream(device: &cpal::Device) -> cpal::Stream {
    let config = device
        .supported_output_configs()
        .expect("No output configs")
        .next()
        .expect("No output configs")
        .with_max_sample_rate()
        .config();

    let stream = device
        .build_output_stream(
            &config,
            |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                data.fill(0.0);
            },
            |err| {
                tracing::error!(?err);
            },
            None,
        )
        .expect("Failed to build output stream");
    let _ = stream.pause();

    stream
}

struct Metadata {
    duration: TimeStamp,
    timebase: TimeBase,
}

struct App {
    device: cpal::Device,
    stream: cpal::Stream,

    position: Arc<Atomic<TimeStamp>>,
    metadata: Option<Metadata>,
}

// TODO: Fix reliance on HW Stream Pausing
impl App {
    pub fn new(_context: &eframe::CreationContext) -> Self {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .expect("No default output device");

        Self {
            stream: empty_stream(&device),
            position: Arc::new(Atomic::new(TimeStamp::default())),
            metadata: None,
            device,
        }
    }

    pub fn request_seek(
        &self,
        pos: Time,
    ) {
        let Some(ref metadata) = self.metadata else {
            return;
        };

        let ts = metadata.timebase.calc_timestamp(pos);
        let _ = self.stream.pause();
        self.position
            .store(ts, std::sync::atomic::Ordering::Relaxed);
    }

    #[allow(clippy::too_many_lines)]
    fn start_symphonia_stream(
        &mut self,
        mut format: Box<dyn FormatReader>,
    ) {
        // FIXME: Error handling
        let track = format
            .default_track(symphonia::core::formats::TrackType::Audio)
            .expect("no default track");
        let track_id = track.id;

        let duration = track.num_frames.expect("No length to track");
        let timebase = track.time_base.expect("Track missing Timebase");
        self.metadata = Some(Metadata { duration, timebase });

        self.position
            .store(track.start_ts, std::sync::atomic::Ordering::SeqCst);

        let mut decoder: Box<dyn AudioDecoder> = symphonia::default::get_codecs()
            .make_audio_decoder(
                track
                    .codec_params
                    .as_ref()
                    .expect("No codec params")
                    .audio()
                    .expect("Track not Audio"),
                &AudioDecoderOptions::default(),
            )
            .expect("Track codec unsupported");
        let codec = decoder.codec_params();

        let available_device = self
            .device
            .supported_output_configs()
            .expect("No output configs")
            .find(|config| {
                config.channels() as usize
                    == codec
                        .channels
                        .clone()
                        .map(|c| c.count())
                        .expect("No channels in Audio file")
                    && config.sample_format() == cpal::SampleFormat::F32
            })
            .expect("No output device with required parameters");

        let supported_device = available_device
            .try_with_sample_rate(cpal::SampleRate(codec.sample_rate.expect("shitface")))
            .unwrap_or_else(|| available_device.with_max_sample_rate());

        let position = self.position.clone();

        let mut buffer: VecDeque<f32> = VecDeque::new();
        self.stream = self
            .device
            .build_output_stream(
                &supported_device.config(),
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    while data.len() > buffer.len() {
                        match format.next_packet() {
                            Ok(Some(packet)) => {
                                if packet.track_id() != track_id {
                                    continue;
                                }

                                if let Err(ts) = position.compare_exchange(
                                    packet.ts,
                                    packet.ts + packet.dur,
                                    std::sync::atomic::Ordering::SeqCst,
                                    std::sync::atomic::Ordering::Acquire,
                                ) {
                                    let _ = format.seek(
                                        symphonia::core::formats::SeekMode::Accurate,
                                        symphonia::core::formats::SeekTo::TimeStamp {
                                            ts,
                                            track_id,
                                        },
                                    );
                                    decoder.reset();
                                }

                                let packet = decoder.decode(&packet).expect("failed to decode");

                                let mut samples =
                                    vec![0.0; packet.samples_interleaved()].into_boxed_slice();
                                packet.copy_to_slice_interleaved(&mut samples);
                                buffer.extend(&samples);
                            },
                            Err(symphonia::core::errors::Error::ResetRequired) => {
                                // Reset decoder
                                trace!(track_id, "Decoder reset");
                                decoder.reset();
                            },
                            Err(_) | Ok(None) => {
                                // Side-effect of doing it like this is that if a song finishes, we
                                // cannot seek
                                while data.len() > buffer.len() {
                                    buffer.push_back(0.0);
                                }
                            },
                        }
                    }

                    for (index, sample) in buffer.drain(..data.len()).enumerate() {
                        data[index] = sample;
                    }
                },
                move |err| {
                    tracing::error!(?err);
                },
                None,
            )
            .expect("failed to create stream");
    }
}

impl eframe::App for App {
    #[allow(clippy::too_many_lines)]
    fn update(
        &mut self,
        ctx: &egui::Context,
        _frame: &mut eframe::Frame,
    ) {
        if !ctx.has_requested_repaint() {
            ctx.request_repaint();
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            let selected_name = self.device.name().expect("Audio device has no name");
            egui::ComboBox::from_label("Devices")
                .selected_text(&selected_name)
                .show_ui(ui, |ui| {
                    for device in cpal::default_host()
                        .output_devices()
                        .expect("No output devices")
                    {
                        let label = device.name().expect("Audio Device has no name");
                        if ui
                            .selectable_label(
                                label == selected_name,
                                device.name().expect("Audio Device has no name"),
                            )
                            .clicked()
                        {
                            self.device = device;
                        }
                    }
                });

            if let Some(ref metadata) = self.metadata {
                ui.horizontal(|ui| {
                    let current = self.position.load(std::sync::atomic::Ordering::SeqCst);

                    let width = ui.max_rect().width();
                    let prev_width = ui.data(|data| {
                        data.get_temp(egui::Id::new("progress_prev_width"))
                            .unwrap_or(width)
                    });
                    let target_width = width - prev_width;

                    let current_sec = metadata.timebase.calc_time(current).seconds;
                    let duration_sec = metadata.timebase.calc_time(metadata.duration).seconds;
                    ui.label(format!("{current_sec}"));
                    ui.add(
                        egui::ProgressBar::new(current as f32 / metadata.duration as f32)
                            .desired_width(target_width),
                    );
                    ui.label(format!("{duration_sec}"));

                    ui.data_mut(|data| {
                        data.insert_temp(
                            egui::Id::new("progress_prev_width"),
                            ui.min_rect().width() - target_width,
                        );
                    });
                });
            }

            ui.label("Hello world!");
            if ui.button("Open file").clicked() {
                if let Some(picked) = rfd::FileDialog::new()
                    .add_filter("music", &[
                        "wav", "ogg", "mkv", "m4a", "mp3", "flac", "alac",
                    ])
                    .pick_file()
                {
                    // Select file
                    let file = std::fs::File::open(&picked).expect("Failed to open selected file");
                    let mss =
                        MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
                    let mut hint = Hint::new();
                    if let Some(ext) = picked.extension().and_then(|s| s.to_str()) {
                        hint.with_extension(ext);
                    }

                    // Figure out what it has
                    let format = symphonia::default::get_probe()
                        .probe(
                            &hint,
                            mss,
                            FormatOptions {
                                enable_gapless: true,
                                prebuild_seek_index: true,
                                ..Default::default()
                            },
                            MetadataOptions::default(),
                        )
                        .expect("Failed to probe file");

                    // TODO: MPRIS over (z)bus w/ metadata (source agnostic required)

                    self.start_symphonia_stream(format);
                }
            }
        });
    }
}
