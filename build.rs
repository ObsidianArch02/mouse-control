use std::env;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=assets/Info.plist");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
        let info_plist = Path::new(&manifest_dir).join("assets").join("Info.plist");
        println!(
            "cargo:rustc-link-arg-bin=mouse-control=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            info_plist.display()
        );
    }
}
