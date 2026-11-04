//! Windows: puts the MiniVNC icon into the executable.

fn main() {
    println!("cargo:rerun-if-changed=minivnc-viewer.rc");
    println!("cargo:rerun-if-changed=../../assets/minivnc.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("minivnc-viewer.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("compiling the icon resource");
    }
}
