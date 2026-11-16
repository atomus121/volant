use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/volant.rc");
    println!("cargo:rerun-if-changed=assets/volant-icon.ico");
    println!("cargo:rerun-if-changed=assets/volant-icon.svg");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let assets = manifest.join("assets");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let resource = out.join("volant.res");
    let status = Command::new("rc.exe")
        .current_dir(&assets)
        .args(["/nologo", "/fo", resource.to_str().unwrap(), "volant.rc"])
        .status()
        .expect("rc.exe is required to embed the Volant icon resource");
    if !status.success() {
        panic!("rc.exe failed to compile assets/volant.rc");
    }
    println!("cargo:rustc-link-arg={}", resource.display());
}
