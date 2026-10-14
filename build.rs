use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=Cargo.toml");
    // Embed resources only when really building on Windows: cross-checks from macOS/Linux have no rc.exe
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") && cfg!(windows) {
        let rc = PathBuf::from(env::var("OUT_DIR").unwrap()).join("app.rc");
        fs::write(&rc, resource_script()).expect("cannot write app.rc");
        embed_resource::compile(&rc, embed_resource::NONE)
            .manifest_optional()
            .expect("cannot compile Windows resources");
    }
}

/// Icon ordinal 1 (Explorer, taskbar, Alt+Tab, and the tray via Icon::from_resource)
/// plus VERSIONINFO so exe Properties show name/version: generated from Cargo.toml so it always matches the tag
fn resource_script() -> String {
    let icon = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets/icon.ico");
    let icon = icon.display().to_string().replace('\\', "\\\\");
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let part = |name| env::var(name).unwrap();
    let numeric = format!(
        "{},{},{},0",
        part("CARGO_PKG_VERSION_MAJOR"),
        part("CARGO_PKG_VERSION_MINOR"),
        part("CARGO_PKG_VERSION_PATCH")
    );
    format!(
        r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "pawaretdev"
      VALUE "FileDescription", "Gamepulse"
      VALUE "FileVersion", "{version}"
      VALUE "LegalCopyright", "Copyright (c) 2026 pawaretdev. MIT License."
      VALUE "ProductName", "Gamepulse"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    )
}
