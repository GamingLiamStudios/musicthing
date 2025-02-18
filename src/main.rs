use std::{
    collections::VecDeque,
    time::Duration,
};

use cpal::traits::{
    DeviceTrait,
    HostTrait,
    StreamTrait,
};
use eframe::NativeOptions;
use symphonia::core::{
    audio::{
        AudioBuffer,
        AudioBufferRef,
        Signal,
    },
    codecs::{
        Decoder,
        DecoderOptions,
    },
    formats::FormatOptions,
    io::{
        MediaSourceStream,
        MediaSourceStreamOptions,
    },
    meta::MetadataOptions,
    probe::{
        Hint,
        ProbeResult,
    },
};
use tracing::info;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fmt_subscriber = tracing_subscriber::fmt::Subscriber::builder()
        .with_max_level(tracing::Level::DEBUG)
        .finish();
    tracing::subscriber::set_global_default(fmt_subscriber)?;

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

struct App {
    audio_device: cpal::Device,

    current_stream: Option<cpal::Stream>,
}

impl App {
    pub fn new(_context: &eframe::CreationContext) -> Self {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .expect("No default output device");

        Self {
            audio_device:   device,
            current_stream: None,
        }
    }
}

impl eframe::App for App {
    #[allow(clippy::too_many_lines)]
    fn update(
        &mut self,
        ctx: &egui::Context,
        _frame: &mut eframe::Frame,
    ) {
        egui::CentralPanel::default().show(ctx, |ui| {
            let selected_name = self.audio_device.name().expect("Audio device has no name");
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
                            self.audio_device = device;
                        }
                    }
                });

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
                    let ProbeResult {
                        mut format,
                        metadata: _,
                    } = symphonia::default::get_probe()
                        .format(
                            &hint,
                            mss,
                            &FormatOptions {
                                enable_gapless: true,
                                prebuild_seek_index: true,
                                ..Default::default()
                            },
                            &MetadataOptions::default(),
                        )
                        .expect("Failed to probe file");

                    let track = format.default_track().expect("no default track");
                    let track_id = track.id;
                    let mut decoder: Box<dyn Decoder> = symphonia::default::get_codecs()
                        .make(&track.codec_params, &DecoderOptions::default())
                        .expect("e");

                    // Create an audio device
                    let supported_device = self
                        .audio_device
                        .supported_output_configs()
                        .expect("No output configs")
                        .filter(|config| {
                            config.channels() as usize
                                == track
                                    .codec_params
                                    .channels
                                    .expect("No channels in file")
                                    .count()
                                && config.sample_format() == cpal::SampleFormat::F32
                        })
                        .find_map(|config| {
                            config.try_with_sample_rate(cpal::SampleRate(
                                track.codec_params.sample_rate.expect("shitface"),
                            ))
                        })
                        .expect("winblows");

                    let mut buffer: VecDeque<f32> = VecDeque::new();
                    let stream = self
                        .audio_device
                        .build_output_stream(
                            &supported_device.config(),
                            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                                while data.len() > buffer.len() {
                                    let mut filled = false;
                                    while let Ok(packet) = format.next_packet() {
                                        if packet.track_id() != track_id {
                                            continue;
                                        }

                                        let packet =
                                            decoder.decode(&packet).expect("failed to decode");
                                        let mut audio: AudioBuffer<f32> = packet.make_equivalent();
                                        packet.convert(&mut audio);

                                        // Audio buffer should be interlaced
                                        for sample in 0..audio.capacity() {
                                            for channel in 0..audio.spec().channels.count() {
                                                buffer.push_back(audio.chan(channel)[sample]);
                                            }
                                        }
                                        filled = true;
                                        break;
                                    }

                                    if !filled {
                                        for _ in 0..data.len() {
                                            buffer.push_back(0.0);
                                        }
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
                    self.current_stream = Some(stream);
                }
            }
        });
    }
}
