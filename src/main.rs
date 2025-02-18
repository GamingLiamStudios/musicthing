use eframe::NativeOptions;
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
                builder.with_title("musicthing").with_app_id("floating")
            })),
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )?;
    Ok(())
}

struct App {}

impl App {
    pub const fn new(_context: &eframe::CreationContext) -> Self {
        Self {}
    }
}

impl eframe::App for App {
    fn update(
        &mut self,
        ctx: &egui::Context,
        _frame: &mut eframe::Frame,
    ) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label("Hello world!");
            if ui.button("Shit").clicked() {
                info!("face");
            }
        });
    }
}
