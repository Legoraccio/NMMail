//! Embeds the application manifest (DPI awareness, Common Controls v6) and
//! the tray icons into nmmaild.exe on Windows.

fn main() {
    #[cfg(windows)]
    windows_resources();
}

#[cfg(windows)]
fn windows_resources() {
    use std::path::PathBuf;

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let assets = manifest_dir.join("..").join("..").join("assets").join("icon");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let esc = |p: &std::path::Path| p.to_string_lossy().replace('\\', "\\\\");

    let mut rc = String::new();
    for (id, name) in [(1, "nmmail.ico"), (2, "nmmail-unread.ico"), (3, "nmmail-offline.ico")] {
        let p = assets.join(name);
        println!("cargo:rerun-if-changed={}", p.display());
        if p.exists() {
            rc.push_str(&format!("{id} ICON \"{}\"\n", esc(&p)));
        }
    }
    let manifest = manifest_dir.join("nmmaild.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());
    rc.push_str(&format!("1 24 \"{}\"\n", esc(&manifest)));
    let rc_path = out.join("nmmaild.rc");
    std::fs::write(&rc_path, rc).unwrap();
    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_optional()
        .unwrap();
}
