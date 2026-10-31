use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=assets/lumen-diff.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let icon =
            PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("assets/lumen-diff.ico");
        let rc = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("lumen-icon.rc");
        fs::write(
            &rc,
            format!(
                "1 ICON \"{}\"\n",
                icon.display().to_string().replace('\\', "\\\\")
            ),
        )
        .unwrap();
        embed_resource::compile(&rc, embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
    println!("cargo:rerun-if-changed=native/raw.cpp");
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
        .file("native/raw.cpp");
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
