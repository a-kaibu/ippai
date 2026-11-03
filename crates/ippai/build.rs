// The version `ippai --version` prints: IPPAI_VERSION (the release workflow sets the tag), else
// the crate's. The web app is built in (webassets.rs): a new web/dist rebuilds the binary.
fn main() {
    println!("cargo:rerun-if-env-changed=IPPAI_VERSION");
    println!("cargo:rerun-if-changed=../../web/dist");
    let v = std::env::var("IPPAI_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
    println!("cargo:rustc-env=IPPAI_VERSION={v}");
}
