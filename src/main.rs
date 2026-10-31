mod benchmark;
mod decode;
mod gpu;
mod help;
mod model;
mod platform;
mod processing;
mod services;
mod ui;

extern crate libz_sys;

use clap::Parser;
use gpui_kit::{px, size, App, AppContext, Bounds, WindowBounds, WindowOptions};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Compare images and Nikon HE/HE* RAWs with DX12 compute"
)]
struct Args {
    /// Image paths and/or directories. Paths with spaces must be quoted.
    paths: Vec<PathBuf>,
    /// Include subdirectories (symbolic links are never followed).
    #[arg(short, long)]
    recursive: bool,
    /// Disable GPU compute and use the background Rayon renderer.
    #[arg(long)]
    cpu: bool,
    /// Override the thumbnail cache directory.
    #[arg(long)]
    cache_dir: Option<PathBuf>,
    /// Print a 45 MP compute benchmark as JSON (or use the first two paths).
    #[arg(long)]
    benchmark: bool,
    #[arg(long, default_value_t = 20)]
    iterations: usize,
    /// Decode each path and its thumbnail, printing diagnostics without a window.
    #[arg(long)]
    probe: bool,
}

fn main() {
    let args = Args::parse();
    if args.probe {
        if let Err(error) = benchmark::probe(&args.paths) {
            eprintln!("Decode failed: {error:#}");
            std::process::exit(1);
        }
        return;
    }
    if args.benchmark {
        if let Err(error) = benchmark::run(&args.paths, args.cpu, args.iterations) {
            eprintln!("Benchmark failed: {error:#}");
            std::process::exit(1);
        }
        return;
    }
    let cache = args.cache_dir.unwrap_or_else(|| {
        directories::ProjectDirs::from("app", "Lumen", "LumenDiff")
            .map(|p| p.cache_dir().join("thumbnails"))
            .unwrap_or_else(|| std::env::temp_dir().join("lumen-diff-thumbnails"))
    });
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            cx.set_window_appearance(Some(gpui_kit::WindowAppearance::Dark));
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
            gpui_kit::component::Theme::update(cx, |theme| {
                theme.primary = gpui_kit::rgb(0xe66a56).into();
                theme.primary_hover = gpui_kit::rgb(0xee7d69).into();
                theme.primary_active = gpui_kit::rgb(0xc75443).into();
                theme.primary_foreground = gpui_kit::rgb(0x211917).into();
                theme.slider_bar = gpui_kit::rgb(0xad7567).into();
                theme.slider_thumb = gpui_kit::rgb(0xe4d5ce).into();
                theme.focus_ring = false;
                theme.radius = px(6.);
            });
            let bounds = Bounds::centered(None, size(px(1500.), px(960.)), cx);
            let result = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(1120.), px(700.))),
                    titlebar: Some(gpui_kit::TitlebarOptions {
                        title: Some("Lumen Diff".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    platform::style_window(window);
                    cx.new(|cx| {
                        ui::Desk::new(args.paths, args.recursive, args.cpu, cache, window, cx)
                    })
                },
            );
            if let Err(error) = result {
                eprintln!("Could not open window: {error:#}");
                cx.quit();
            }
            cx.activate(true);
        });
}
