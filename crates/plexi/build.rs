//! Embeds the app icon and version information into plexi.exe on Windows.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=../../assets/plexi.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = manifest.join("../../assets/plexi.ico").canonicalize().expect("assets/plexi.ico");
    let icon = icon.to_string_lossy().replace('\\', "/");
    let icon = icon.strip_prefix("//?/").unwrap_or(&icon).to_owned();

    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut parts: Vec<u16> = version.split(['.', '-']).filter_map(|p| p.parse().ok()).collect();
    parts.resize(4, 0);
    let commas = parts.iter().map(u16::to_string).collect::<Vec<_>>().join(",");

    let rc = format!(
        r#"1 ICON "{icon}"

1 VERSIONINFO
FILEVERSION {commas}
PRODUCTVERSION {commas}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Hayden Harms"
      VALUE "FileDescription", "Plexi"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "plexi"
      VALUE "LegalCopyright", "Copyright (c) 2026 Hayden Harms. All rights reserved."
      VALUE "OriginalFilename", "plexi.exe"
      VALUE "ProductName", "Plexi"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("plexi.rc");
    std::fs::write(&out, rc).unwrap();
    if let Err(e) = embed_resource::compile(&out, embed_resource::NONE).manifest_optional() {
        println!("cargo:warning=could not embed the icon: {e}");
    }
}
