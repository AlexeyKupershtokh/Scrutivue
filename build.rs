use std::{env, fs, path::PathBuf};

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let profile = out.ancestors().nth(3).unwrap();
    println!("cargo:rerun-if-changed=assets/models/lpips-squeeze-v0.1.json");
    let model_pin: serde_json::Value =
        serde_json::from_str(&fs::read_to_string("assets/models/lpips-squeeze-v0.1.json").unwrap())
            .unwrap();
    let model_name = model_pin["model"].as_str().unwrap();
    let model = PathBuf::from("assets/models").join(model_name);
    println!("cargo:rerun-if-changed={}", model.display());
    // Runtime assets stay outside the executable, including beside test binaries.
    for directory in [profile.join("models"), profile.join("deps/models")] {
        fs::create_dir_all(&directory).unwrap();
        fs::copy(&model, directory.join(model_name)).expect("copy LPIPS model beside executable");
    }
    println!("cargo:rerun-if-changed=config/onnxruntime.json");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let source = PathBuf::from("vendor/onnxruntime");
        let pin: serde_json::Value =
            serde_json::from_str(&fs::read_to_string("config/onnxruntime.json").unwrap()).unwrap();
        assert_eq!(
            fs::read_to_string(source.join(".diff-pin"))
                .unwrap_or_default()
                .trim(),
            pin["version"].as_str().unwrap(),
            "Run python scripts/prepare.py to fetch ONNX Runtime"
        );
        println!("cargo:rerun-if-changed=vendor/onnxruntime/.diff-pin");
        for directory in [profile.to_path_buf(), profile.join("deps")] {
            fs::create_dir_all(&directory).unwrap();
            fs::copy(
                source.join("lib/onnxruntime.dll"),
                directory.join("onnxruntime.dll"),
            )
            .expect("copy ONNX Runtime beside executable");
        }
    }
    println!("cargo:rerun-if-changed=assets/scrutivue.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let icon =
            PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("assets/scrutivue.ico");
        let rc = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("scrutivue-icon.rc");
        fs::write(
            &rc,
            format!(
                r#"1 ICON "{icon}"
1 VERSIONINFO
 FILEVERSION {major},{minor},{patch},0
 PRODUCTVERSION {major},{minor},{patch},0
 FILETYPE 0x1L
BEGIN
 BLOCK "StringFileInfo"
 BEGIN
  BLOCK "040904B0"
  BEGIN
   VALUE "FileDescription", "Scrutivue photograph and RAW comparison\0"
   VALUE "FileVersion", "{version}\0"
   VALUE "InternalName", "scrutivue\0"
   VALUE "OriginalFilename", "scrutivue.exe\0"
   VALUE "ProductName", "Scrutivue\0"
   VALUE "ProductVersion", "{version}\0"
  END
 END
 BLOCK "VarFileInfo"
 BEGIN
  VALUE "Translation", 0x409, 1200
 END
END
"#,
                icon = icon.display().to_string().replace('\\', "\\\\"),
                major = env::var("CARGO_PKG_VERSION_MAJOR").unwrap(),
                minor = env::var("CARGO_PKG_VERSION_MINOR").unwrap(),
                patch = env::var("CARGO_PKG_VERSION_PATCH").unwrap(),
                version = env::var("CARGO_PKG_VERSION").unwrap(),
            ),
        )
        .unwrap();
        embed_resource::compile(&rc, embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
    println!("cargo:rerun-if-changed=native/raw.cpp");
    println!("cargo:rerun-if-changed=native/parallel_raw.h");
    println!("cargo:rerun-if-changed=native/parallel_raw.cpp");
    println!("cargo:rerun-if-changed=config/libraw.json");
    println!("cargo:rerun-if-env-changed=DIFF_LIBRAW_SOURCE");
    let pin: serde_json::Value =
        serde_json::from_str(&fs::read_to_string("config/libraw.json").unwrap()).unwrap();
    let source = env::var_os("DIFF_LIBRAW_SOURCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("vendor/libraw"));
    let stamp = fs::read_to_string(source.join(".diff-pin")).unwrap_or_default();
    assert_eq!(stamp.trim(), pin["version"].as_str().unwrap(), "Run `python scripts/prepare.py` before building. An upstream LibRaw is NOT a substitute (Nikon HE/HE*).");
    println!(
        "cargo:rerun-if-changed={}",
        source.join(".diff-pin").display()
    );
    let makefile = fs::read_to_string(source.join("Makefile.dist")).unwrap();
    let objects = makefile
        .split("LIB_OBJECTS=")
        .nth(1)
        .expect("LibRaw object list");
    let mut names = Vec::new();
    for line in objects.lines() {
        for token in line.split(|c: char| c.is_whitespace() || c == '\\') {
            if let Some(name) = token
                .strip_prefix("object/")
                .and_then(|x| x.strip_suffix(".o"))
            {
                names.push(format!("{name}.cpp"));
            }
        }
        if !line.trim_end().ends_with('\\') {
            break;
        }
    }
    assert!(!names.is_empty());
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .opt_level(3)
        .warnings(false)
        .define("LIBRAW_NODLL", None)
        .define("USE_ZLIB", None)
        .include(&source)
        .file("native/raw.cpp")
        .file("native/parallel_raw.cpp");
    if let Ok(include) = env::var("DEP_Z_INCLUDE") {
        build.include(include);
    }
    for name in names {
        let file = walkdir::WalkDir::new(source.join("src"))
            .into_iter()
            .filter_map(Result::ok)
            .find(|e| e.file_name().to_str() == Some(&name))
            .unwrap_or_else(|| panic!("Missing {name}"));
        build.file(file.path());
    }
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        build.define("_CRT_SECURE_NO_WARNINGS", None).flag("/EHsc");
        println!("cargo:rustc-link-lib=ws2_32");
    }
    build.compile("diff_raw");
}
