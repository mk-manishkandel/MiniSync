//! Embeds the app icon and version info into minisync.exe on Windows.
//! (macOS gets its icon from the .app bundle instead; see README.)

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/minisync.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico = manifest_dir.join("assets").join("minisync.ico");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let nums: Vec<&str> = version.split(['.', '-']).take(3).collect();
    let comma = format!("{},0", nums.join(","));

    let rc = format!(
        r#"1 ICON "{ico}"
1 VERSIONINFO
FILEVERSION {comma}
PRODUCTVERSION {comma}
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "FileDescription", "minisync - sync folders between your devices"
      VALUE "ProductName", "Mini Sync"
      VALUE "FileVersion", "{version}"
      VALUE "ProductVersion", "{version}"
      VALUE "OriginalFilename", "minisync.exe"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        ico = ico.display().to_string().replace('\\', "/"),
    );
    let rc_path = out_dir.join("minisync.rc");
    let obj_path = out_dir.join("minisync-res.o");
    std::fs::write(&rc_path, rc).unwrap();

    if msvc {
        // Microsoft toolchain (used by the GitHub release build): rc.exe via embed-resource.
        if let Err(e) = embed_resource::compile(&rc_path, embed_resource::NONE).manifest_optional() {
            println!("cargo:warning=could not embed icon ({e}); building without icon");
        }
        return;
    }

    // GNU toolchain (this PC): windres from MinGW.
    let windres = std::env::var("WINDRES").unwrap_or_else(|_| "windres".into());
    let status = Command::new(&windres)
        .arg("--input-format=rc")
        .arg("--output-format=coff")
        .arg("-i")
        .arg(&rc_path)
        .arg("-o")
        .arg(&obj_path)
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("cargo:rustc-link-arg-bins={}", obj_path.display());
        }
        Ok(s) => println!("cargo:warning=windres failed ({s}); building without icon"),
        Err(e) => println!("cargo:warning=windres not found ({e}); building without icon"),
    }
}
