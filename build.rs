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

    // A standard application manifest: runs as the normal user, declares
    // Windows 10/11 support. (Antivirus models trust a bare exe less.)
    let manifest = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="MiniSync" version="1.0.0.0"/>
  <description>Mini Sync</description>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
</assembly>
"#;
    let manifest_path = out_dir.join("minisync.manifest");
    std::fs::write(&manifest_path, manifest).unwrap();

    // The GNU toolchain already embeds a default manifest; a second one fails to link.
    let manifest_line = if msvc {
        format!("1 24 \"{}\"\n", manifest_path.display().to_string().replace('\\', "/"))
    } else {
        String::new()
    };
    let rc = format!(
        r#"1 ICON "{ico}"
{manifest_line}1 VERSIONINFO
FILEVERSION {comma}
PRODUCTVERSION {comma}
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "mk-manishkandel"
      VALUE "FileDescription", "Mini Sync - sync folders between your devices"
      VALUE "LegalCopyright", "Copyright (c) 2026 mk-manishkandel"
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
