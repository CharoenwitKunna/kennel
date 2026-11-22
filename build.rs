// Embeds Kennel's icon (drawn in src/icon.rs) into kennel.exe on Windows.
// If no resource compiler is available the build still succeeds, with a warning.

use std::path::PathBuf;

include!("src/icon.rs");

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico = out.join("kennel.ico");
    std::fs::write(&ico, icon_ico(&[16, 20, 24, 32, 40, 48, 64, 128, 256])).unwrap();
    let rc = out.join("kennel.rc");
    let ico_path = ico.to_string_lossy().replace('\\', "/");
    std::fs::write(&rc, format!("1 ICON \"{ico_path}\"\n")).unwrap();
    match embed_resource::compile(&rc, embed_resource::NONE) {
        embed_resource::CompilationResult::Failed(e) | embed_resource::CompilationResult::NotAttempted(e) => {
            println!("cargo:warning=exe icon not embedded: {e}");
        }
        _ => {}
    }
}
