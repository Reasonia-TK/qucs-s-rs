//! Windows: embed the application icon and version information into the .exe
//! (needs `rc.exe` from the Windows SDK; skipped with a warning when it is missing).

#[allow(dead_code)]
mod icon {
    include!("src/icon_pixels.rs");
}

fn main() {
    println!("cargo:rerun-if-changed=src/icon_pixels.rs");
    println!("cargo:rerun-if-changed=build.rs");
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("windows-msvc") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let ico = out.join("app.ico");
    if let Err(e) = std::fs::write(&ico, ico_file(&[16, 24, 32, 48, 64, 128, 256])) {
        println!("cargo:warning=icon not written: {e}");
        return;
    }
    let Some(rc) = find_rc() else {
        println!("cargo:warning=rc.exe not found: the .exe gets no icon (install the Windows SDK or set RC)");
        return;
    };
    let ver = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let nums: Vec<u32> = ver.split(['.', '-']).take(3).map(|p| p.parse().unwrap_or(0)).collect();
    let (a, b, c) = (nums.first().copied().unwrap_or(0), nums.get(1).copied().unwrap_or(0), nums.get(2).copied().unwrap_or(0));
    let script = format!(
        r#"1 ICON "{ico}"
1 VERSIONINFO
FILEVERSION {a},{b},{c},0
PRODUCTVERSION {a},{b},{c},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "qucs-s-rs"
      VALUE "FileDescription", "Qucs-S RS schematic editor and MCP server"
      VALUE "FileVersion", "{ver}"
      VALUE "InternalName", "qucs-s-rs"
      VALUE "LegalCopyright", "GPL-2.0-or-later"
      VALUE "OriginalFilename", "qucs-s-rs.exe"
      VALUE "ProductName", "Qucs-S RS"
      VALUE "ProductVersion", "{ver}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        ico = ico.display().to_string().replace('\\', "\\\\")
    );
    let rc_file = out.join("app.rc");
    let res = out.join("app.res");
    if std::fs::write(&rc_file, script).is_err() {
        return;
    }
    let ok = std::process::Command::new(&rc).arg("/nologo").arg("/fo").arg(&res).arg(&rc_file).status().map(|s| s.success()).unwrap_or(false);
    if ok {
        println!("cargo:rustc-link-arg-bins={}", res.display());
    } else {
        println!("cargo:warning=rc.exe failed: the .exe gets no icon");
    }
}

/// `rc.exe`: $RC, PATH, or the newest Windows 10/11 SDK.
fn find_rc() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("RC").map(std::path::PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    if let Some(path) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&path) {
            let p = d.join("rc.exe");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    let base = std::path::Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut vers: Vec<std::path::PathBuf> = std::fs::read_dir(base).ok()?.flatten().map(|e| e.path()).filter(|p| p.join("x64").join("rc.exe").is_file()).collect();
    vers.sort();
    vers.pop().map(|v| v.join("x64").join("rc.exe"))
}

/// ICO file with 32-bit BMP images at the given sizes.
fn ico_file(sizes: &[usize]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&n| bmp_entry(n)).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len();
    for (n, img) in sizes.iter().zip(&images) {
        let d = if *n >= 256 { 0 } else { *n as u8 };
        out.extend_from_slice(&[d, d, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += img.len();
    }
    for img in images {
        out.extend_from_slice(&img);
    }
    out
}

/// BITMAPINFOHEADER + bottom-up BGRA pixels + AND mask (all opaque via alpha).
fn bmp_entry(n: usize) -> Vec<u8> {
    let rgba = icon::icon_rgba(n);
    let mut b = Vec::new();
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&(n as i32).to_le_bytes());
    b.extend_from_slice(&((2 * n) as i32).to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(&[0u8; 24]);
    for y in (0..n).rev() {
        for x in 0..n {
            let o = (y * n + x) * 4;
            b.extend_from_slice(&[rgba[o + 2], rgba[o + 1], rgba[o], rgba[o + 3]]);
        }
    }
    let row = n.div_ceil(32) * 4;
    b.extend(std::iter::repeat_n(0u8, row * n));
    b
}
