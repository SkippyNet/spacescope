#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod fonts;
mod icons;
mod logo;
mod platform;
mod script;
mod start;
mod theme;
mod treemap;
mod watch;
mod worker;

pub const APP_NAME: &str = "SpaceScope";
const RENDERER_ENV: &str = "SPACESCOPE_RENDERER";

fn run(args: app::Args, glow: bool) -> eframe::Result {
    let icon = egui::IconData {
        rgba: logo::rgba(128),
        width: 128,
        height: 128,
    };
    let (w, h) = args.size.unwrap_or((1280.0, 800.0));
    let viewport = egui::ViewportBuilder::default()
        .with_title(APP_NAME)
        .with_app_id("spacescope")
        .with_inner_size([w, h])
        .with_min_inner_size([680.0, 440.0])
        .with_icon(icon);
    let mut options = eframe::NativeOptions {
        viewport,
        renderer: if glow {
            eframe::Renderer::Glow
        } else {
            eframe::Renderer::Wgpu
        },
        // Test runs must not inherit or disturb the saved window geometry.
        persist_window: args.screenshot.is_none() && args.script.is_none() && args.size.is_none(),
        ..Default::default()
    };
    // A disk tool has no business waking the discrete GPU.
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup {
        setup.power_preference = eframe::wgpu::PowerPreference::LowPower;
    }
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, args)))),
    )
}

fn main() {
    let args = match app::Args::parse() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };
    if let Some(dir) = &args.write_assets {
        if let Err(e) = logo::write_assets(dir) {
            eprintln!("cannot write assets: {e}");
            std::process::exit(1);
        }
        return;
    }

    // OpenGL is the default: measured on Windows it needs about a third of
    // the memory of the Vulkan / Direct3D 12 path for identical output.
    // SPACESCOPE_RENDERER=wgpu (or =gl) pins the choice.
    let choice = std::env::var(RENDERER_ENV).map(|v| v.to_lowercase()).ok();
    let use_glow = !matches!(choice.as_deref(), Some("wgpu" | "vulkan" | "dx12" | "gpu"));
    let Err(err) = run(args, use_glow) else {
        return;
    };
    eprintln!("{APP_NAME}: {err}");

    // OpenGL could not start (remote session, VM without GL drivers, ...).
    // The event loop cannot be rebuilt in-process, so restart once on wgpu,
    // which can fall back to Direct3D's software rasteriser or Vulkan.
    if choice.is_none() {
        let relaunched = std::env::current_exe().and_then(|exe| {
            std::process::Command::new(exe)
                .args(std::env::args_os().skip(1))
                .env(RENDERER_ENV, "wgpu")
                .spawn()
        });
        if relaunched.is_ok() {
            return;
        }
    }
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(APP_NAME)
        .set_description(format!("{APP_NAME} could not open a window:\n\n{err}"))
        .show();
    std::process::exit(1);
}
