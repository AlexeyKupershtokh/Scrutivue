mod benchmark;
mod color;
mod decode;
mod dng;
mod file_actions;
mod gpu;
mod help;
mod jxl;
mod lfu;
mod metrics;
mod minimap;
mod model;
mod noise;
mod platform;
mod preferences;
mod processing;
mod services;
mod streaming;
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
    /// Override the settings JSON path (portable/test installation).
    #[arg(long)]
    settings_file: Option<PathBuf>,
    /// Print a 45 MP compute benchmark as JSON (or use the first two paths).
    #[arg(long)]
    benchmark: bool,
    #[arg(long, default_value_t = 20)]
    iterations: usize,
    /// Decode each path and its thumbnail, printing diagnostics without a window.
    #[arg(long)]
    probe: bool,
    /// Exercise visible DNG tiles, pan reuse and exact idle completion without a window.
    #[arg(long)]
    stream_probe: bool,
    /// Validate partial DNG selection return, pause preservation and explicit clear.
    #[arg(long)]
    stream_resume_probe: bool,
    /// Validate paired tile loading and preview composition without a window.
    #[arg(long, value_parser = ["slider", "diff", "slider-fit", "diff-fit"])]
    stream_pair_probe: Option<String>,
    /// Save paired-probe preview buffers in a new directory (never overwrite).
    #[arg(long, requires = "stream_pair_probe")]
    stream_probe_output: Option<PathBuf>,
    /// Print full-frame pixel errors and perceptual metrics for two paths as JSON.
    #[arg(long)]
    statistics: bool,
    /// Long edge of perceptual analysis copies (32..1536); pixel errors stay full size.
    #[arg(long, default_value_t = 1536, value_parser = clap::value_parser!(u32).range(32..=1536))]
    metric_edge: u32,
}

fn main() {
    let args = Args::parse();
    if args.stream_resume_probe {
        let result = (|| -> anyhow::Result<_> {
            anyhow::ensure!(
                args.paths.len() == 2,
                "A selection-return probe requires exactly two input files"
            );
            streaming::probe_resume([args.paths[0].clone(), args.paths[1].clone()])
        })();
        match result {
            Ok(report) => println!("{report}"),
            Err(error) => {
                eprintln!("DNG selection-return probe failed: {error:#}");
                std::process::exit(1);
            }
        }
        return;
    }
    if let Some(mode) = &args.stream_pair_probe {
        let result = (|| -> anyhow::Result<_> {
            anyhow::ensure!(
                args.paths.len() == 2,
                "A paired streaming probe requires exactly two input files"
            );
            streaming::probe_pair(
                [args.paths[0].clone(), args.paths[1].clone()],
                if mode.starts_with("diff") {
                    model::Mode::Diff
                } else {
                    model::Mode::Slider
                },
                mode.ends_with("-fit"),
                args.stream_probe_output.as_deref(),
            )
        })();
        match result {
            Ok(report) => println!("{report}"),
            Err(error) => {
                eprintln!("Paired DNG streaming probe failed: {error:#}");
                std::process::exit(1);
            }
        }
        return;
    }
    if args.stream_probe {
        for path in &args.paths {
            match streaming::probe(path) {
                Ok(report) => println!("{}", report),
                Err(error) => {
                    eprintln!("DNG streaming probe failed: {error:#}");
                    std::process::exit(1);
                }
            }
        }
        return;
    }
    if args.statistics {
        let result = (|| -> anyhow::Result<()> {
            anyhow::ensure!(
                args.paths.len() == 2,
                "--statistics requires exactly two image paths"
            );
            let a = decode::decode(&args.paths[0])?;
            let b = decode::decode(&args.paths[1])?;
            let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
            let report =
                pool.install(|| metrics::compute_complete(&a, &b, args.metric_edge, &|| false))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!("Statistics failed: {error:#}");
            std::process::exit(1);
        }
        return;
    }
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
        directories::ProjectDirs::from("app", "Scrutivue", "Scrutivue")
            .map(|p| p.cache_dir().join("thumbnails"))
            .unwrap_or_else(|| std::env::temp_dir().join("scrutivue-thumbnails"))
    });
    let use_legacy_settings = args.settings_file.is_none();
    let settings_path = args.settings_file.unwrap_or_else(preferences::default_path);
    let (prefs, settings_enabled, settings_warning) =
        match preferences::load_initial(&settings_path, use_legacy_settings) {
            Ok(p) => (p, true, String::new()),
            Err(e) => (
                preferences::Preferences::default(),
                false,
                format!("Could not load settings; the file was left unchanged: {e:#}"),
            ),
        };
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
                theme.primary = gpui_kit::rgb(0x6b6b76).into();
                theme.primary_hover = gpui_kit::rgb(0x7b7b86).into();
                theme.primary_active = gpui_kit::rgb(0x5e5e6a).into();
                theme.primary_foreground = gpui_kit::rgb(0xffffff).into();
                // Shared GPUI tokens cover default buttons, ghost controls and menus.
                // Neutral hover and a brighter selected fill remain visibly distinct.
                theme.foreground = gpui_kit::rgb(0xe5e5e7).into();
                theme.button = gpui_kit::rgb(0x3c3c41).into();
                theme.button_hover = gpui_kit::rgb(0x515158).into();
                theme.button_active = gpui_kit::rgb(0x6b6b76).into();
                theme.button_foreground = gpui_kit::rgb(0xf0f0f2).into();
                theme.secondary = gpui_kit::rgb(0x3c3c41).into();
                theme.secondary_hover = gpui_kit::rgb(0x515158).into();
                theme.secondary_active = gpui_kit::rgb(0x6b6b76).into();
                theme.secondary_foreground = gpui_kit::rgb(0xf0f0f2).into();
                theme.button_secondary = theme.secondary;
                theme.button_secondary_hover = theme.secondary_hover;
                theme.button_secondary_active = theme.secondary_active;
                theme.button_secondary_foreground = theme.secondary_foreground;
                theme.button_primary = theme.primary;
                theme.button_primary_hover = theme.primary_hover;
                theme.button_primary_active = theme.primary_active;
                theme.button_primary_foreground = theme.primary_foreground;
                theme.accent = gpui_kit::rgb(0x77777f).into();
                theme.accent_foreground = gpui_kit::rgb(0xffffff).into();
                theme.border = gpui_kit::rgb(0x606069).into();
                theme.input = gpui_kit::rgb(0x606069).into();
                theme.slider_bar = gpui_kit::rgb(0x92929c).into();
                theme.slider_thumb = gpui_kit::rgb(0xe1e1e6).into();
                theme.focus_ring = false;
                theme.radius = px(6.);
            });
            let bounds = Bounds::centered(
                None,
                size(px(prefs.window_size[0]), px(prefs.window_size[1])),
                cx,
            );
            let result = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(1120.), px(700.))),
                    titlebar: Some(gpui_kit::TitlebarOptions {
                        title: Some("Scrutivue".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    platform::style_window(window);
                    cx.new(|cx| {
                        ui::Desk::new(
                            args.paths,
                            args.recursive,
                            args.cpu,
                            cache,
                            prefs,
                            preferences::Store::new(settings_path, settings_enabled),
                            settings_warning,
                            window,
                            cx,
                        )
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
