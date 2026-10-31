//! Versioned preferences, separate from source images and decode caches.
use crate::model::{Camera, Mode, Settings, Zoom};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub version: u32,
    pub mode: Mode,
    pub gain: u32,
    pub sigma: f32,
    pub highpass: bool,
    pub split: f32,
    pub camera: Camera,
    pub library_open: bool,
    pub recursive: bool,
    pub metrics_open: bool,
    pub metrics_edge: u32,
    pub picker_enabled: bool,
    pub window_size: [f32; 2],
}
impl Default for Preferences {
    fn default() -> Self {
        let s = Settings::default();
        Self {
            version: 1,
            mode: s.mode,
            gain: s.gain,
            sigma: s.sigma,
            highpass: s.highpass,
            split: s.split,
            camera: s.camera,
            library_open: true,
            recursive: false,
            metrics_open: false,
            metrics_edge: 1536,
            picker_enabled: false,
            window_size: [1500., 960.],
        }
    }
}
impl Preferences {
    fn validate(&mut self) {
        let d = Self::default();
        if ![1, 4, 16, 64, 256].contains(&self.gain) {
            self.gain = d.gain;
        }
        self.sigma = finite_clamp(self.sigma, 0.3, 20., d.sigma);
        self.split = finite_clamp(self.split, 0., 1., d.split);
        if let Zoom::Fixed(scale) = &mut self.camera.zoom {
            *scale = finite_clamp(*scale, 0.000001, 8., 1.);
        }
        for v in &mut self.camera.center {
            *v = finite_clamp(*v, -100., 100., 0.5);
        }
        self.metrics_edge = if self.metrics_edge <= 512 { 512 } else { 1536 };
        for i in 0..2 {
            self.window_size[i] = finite_clamp(
                self.window_size[i],
                [1120., 700.][i],
                8192.,
                d.window_size[i],
            );
        }
    }
    pub fn settings(&self) -> Settings {
        Settings {
            // Startup is always Single. `mode` remains the remembered user
            // choice and is restored only after explicitly selecting B.
            mode: Mode::Click,
            gain: self.gain,
            sigma: self.sigma,
            highpass: self.highpass,
            split: self.split,
            camera: self.camera,
            ..Settings::default()
        }
    }
}
fn finite_clamp(v: f32, lo: f32, hi: f32, default: f32) -> f32 {
    if v.is_finite() {
        v.clamp(lo, hi)
    } else {
        default
    }
}
pub fn default_path() -> PathBuf {
    directories::ProjectDirs::from("app", "Scrutivue", "Scrutivue")
        .map(|d| d.config_dir().join("settings.json"))
        .unwrap_or_else(|| PathBuf::from("scrutivue-settings.json"))
}
pub fn load_initial(path: &Path, use_legacy: bool) -> Result<Preferences> {
    let legacy = use_legacy.then(|| {
        directories::ProjectDirs::from("app", "Lumen", "LumenDiff")
            .map(|d| d.config_dir().join("settings.json"))
            .unwrap_or_else(|| PathBuf::from("lumen-diff-settings.json"))
    });
    load_with_fallback(path, legacy.as_deref())
}
fn load_with_fallback(path: &Path, legacy: Option<&Path>) -> Result<Preferences> {
    // Import preferences once, but never modify or delete the old application's file.
    // Explicit --settings-file profiles do not import unrelated preferences.
    if !path.try_exists()? {
        if let Some(legacy) = legacy {
            if legacy.try_exists()? {
                return load(legacy);
            }
        }
    }
    load(path)
}
pub fn load(path: &Path) -> Result<Preferences> {
    let mut p: Preferences = match std::fs::read(path) {
        Ok(bytes) => {
            anyhow::ensure!(bytes.len() <= 65536, "Settings file too large");
            serde_json::from_slice(&bytes).context("Invalid settings JSON")?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Preferences::default(),
        Err(e) => return Err(e.into()),
    };
    anyhow::ensure!(p.version == 1, "Unsupported settings version {}", p.version);
    p.validate();
    Ok(p)
}
fn save(path: &Path, p: &Preferences) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let filename = path
        .file_name()
        .context("Settings path has no filename")?
        .to_string_lossy();
    let temporary = parent.join(format!(
        ".{filename}.{}.{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let result = (|| -> Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        f.write_all(&serde_json::to_vec_pretty(p)?)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
enum Job {
    Save(Preferences),
    Flush(crossbeam_channel::Sender<()>),
    Stop,
}
pub struct Store {
    sender: crossbeam_channel::Sender<Job>,
    pub errors: crossbeam_channel::Receiver<String>,
    worker: Option<std::thread::JoinHandle<()>>,
    last: Option<Preferences>,
    pending: Option<Instant>,
}
impl Store {
    pub fn new(path: PathBuf, enabled: bool) -> Self {
        let (sender, jobs) = crossbeam_channel::unbounded();
        let (error_tx, errors) = crossbeam_channel::bounded(1);
        let worker = std::thread::Builder::new()
            .name("save-settings".into())
            .spawn(move || {
                while let Ok(job) = jobs.recv() {
                    match job {
                        Job::Save(p) if enabled => {
                            if let Err(e) = save(&path, &p) {
                                let _ =
                                    error_tx.try_send(format!("Could not save settings: {e:#}"));
                            }
                        }
                        Job::Save(_) => {}
                        Job::Flush(done) => {
                            let _ = done.send(());
                        }
                        Job::Stop => break,
                    }
                }
            })
            .expect("settings thread");
        Self {
            sender,
            errors,
            worker: Some(worker),
            last: None,
            pending: None,
        }
    }
    pub fn update(&mut self, p: Preferences) {
        if self.last.as_ref() != Some(&p) {
            self.last = Some(p);
            self.pending = Some(Instant::now());
        }
        if self
            .pending
            .is_some_and(|t| t.elapsed() >= Duration::from_millis(400))
        {
            self.submit();
        }
    }
    fn submit(&mut self) {
        if self.pending.take().is_some() {
            if let Some(p) = &self.last {
                let _ = self.sender.send(Job::Save(p.clone()));
            }
        }
    }
    pub fn flush(&mut self, p: Preferences) {
        self.update(p);
        self.submit();
        let (tx, rx) = crossbeam_channel::bounded(1);
        let _ = self.sender.send(Job::Flush(tx));
        let _ = rx.recv();
    }
}
impl Drop for Store {
    fn drop(&mut self) {
        self.submit();
        let _ = self.sender.send(Job::Stop);
        if let Some(t) = self.worker.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "target/settings-tests/{}/{name}.json",
            std::process::id()
        ))
    }
    #[test]
    fn missing_defaults_and_validation() {
        assert_eq!(load(&path("missing")).unwrap(), Preferences::default());
        let mut p = Preferences {
            gain: 999,
            sigma: -1.,
            split: 2.,
            metrics_edge: 17,
            ..Preferences::default()
        };
        p.validate();
        assert_eq!(p.gain, 16);
        assert_eq!(p.sigma, 0.3);
        assert_eq!(p.split, 1.);
        assert_eq!(p.metrics_edge, 512);
        assert_eq!(finite_clamp(f32::NAN, 0., 1., 0.5), 0.5);
    }
    #[test]
    fn legacy_import_preserves_the_original_and_never_overrides_a_new_profile() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let legacy = path(&format!("legacy-import-old-{stamp}"));
        let current = path(&format!("legacy-import-new-{stamp}"));
        let old = Preferences {
            gain: 64,
            ..Preferences::default()
        };
        save(&legacy, &old).unwrap();
        let bytes = std::fs::read(&legacy).unwrap();
        assert_eq!(load_with_fallback(&current, Some(&legacy)).unwrap(), old);
        assert!(!current.exists());
        let new = Preferences {
            gain: 256,
            ..Preferences::default()
        };
        save(&current, &new).unwrap();
        assert_eq!(load_with_fallback(&current, Some(&legacy)).unwrap(), new);
        assert_eq!(std::fs::read(&legacy).unwrap(), bytes);
        assert_eq!(
            load_with_fallback(&path(&format!("isolated-import-{stamp}")), None).unwrap(),
            Preferences::default()
        );
    }
    #[test]
    fn startup_is_single_without_overwriting_the_remembered_comparison_mode() {
        for mode in [Mode::Click, Mode::Slider, Mode::Diff] {
            let p = Preferences {
                mode,
                ..Preferences::default()
            };
            assert_eq!(p.settings().mode, Mode::Click);
            assert_eq!(p.mode, mode);
        }
    }
    #[test]
    fn restart_roundtrip_and_atomic_replacement() {
        let file = path("roundtrip");
        let p = Preferences {
            mode: Mode::Click,
            gain: 64,
            highpass: true,
            recursive: true,
            library_open: false,
            camera: Camera {
                zoom: Zoom::Fixed(4.),
                center: [0.2, 0.7],
            },
            ..Preferences::default()
        };
        {
            let mut store = Store::new(file.clone(), true);
            store.flush(p.clone());
        }
        assert_eq!(load(&file).unwrap(), p);
        let next = Preferences {
            picker_enabled: true,
            ..p
        };
        save(&file, &next).unwrap();
        assert_eq!(load(&file).unwrap(), next);
    }
    #[test]
    fn corrupt_or_future_settings_are_not_overwritten() {
        let file = path("invalid");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        for bytes in [b"not json".as_slice(), b"{\"version\":99}".as_slice()] {
            std::fs::write(&file, bytes).unwrap();
            assert!(load(&file).is_err());
            {
                let mut store = Store::new(file.clone(), false);
                store.flush(Preferences::default());
            }
            assert_eq!(std::fs::read(&file).unwrap(), bytes);
        }
    }
}
