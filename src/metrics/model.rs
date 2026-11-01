//! Locate and validate the external, pinned LPIPS model on the metrics worker.
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

#[derive(serde::Deserialize)]
struct Pin {
    model: String,
    sha256: String,
}
fn pin() -> Result<Pin> {
    serde_json::from_str(include_str!("../../assets/models/lpips-squeeze-v0.1.json"))
        .context("Invalid bundled LPIPS model metadata")
}
fn model_path(executable: &Path, pin: &Pin) -> PathBuf {
    executable.with_file_name("models").join(&pin.model)
}
fn validate(path: &Path, expected: &str) -> Result<()> {
    let mut file = std::fs::File::open(path).with_context(|| {
        format!(
            "LPIPS model is missing or unreadable: {}. Restore models/ from the distribution",
            path.display()
        )
    })?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .with_context(|| format!("Read LPIPS model: {}", path.display()))?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    anyhow::ensure!(
        format!("{:x}", hash.finalize()) == expected,
        "LPIPS model checksum mismatch: {}. Restore the models folder from the distribution",
        path.display()
    );
    Ok(())
}
pub(super) fn installed() -> Result<PathBuf> {
    let pin = pin()?;
    // Never search the working directory, photo folders or PATH for model data.
    let path = model_path(
        &std::env::current_exe().context("Locate LPIPS model")?,
        &pin,
    );
    validate(&path, &pin.sha256)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_path_is_relative_to_the_executable_not_the_working_directory() {
        let root = Path::new("portable app with spaces").join("another folder");
        assert_eq!(
            model_path(&root.join("scrutivue.exe"), &pin().unwrap()),
            root.join("models/lpips-squeeze-v0.1.onnx")
        );
    }
    #[test]
    fn model_checksum_rejects_missing_and_corrupt_files_without_panicking() {
        let root = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("scrutivue-model-test-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("lpips-squeeze-v0.1.onnx");
        let pin = pin().unwrap();
        assert!(validate(&path, &pin.sha256)
            .unwrap_err()
            .to_string()
            .contains("missing or unreadable"));
        std::fs::write(&path, b"not an ONNX model").unwrap();
        assert!(validate(&path, &pin.sha256)
            .unwrap_err()
            .to_string()
            .contains("checksum mismatch"));
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    #[test]
    fn installed_model_matches_the_recorded_pin() {
        assert!(installed().unwrap().is_file());
    }
}
