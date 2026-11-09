// The version `ippai --version` prints: IPPAI_VERSION (the release workflow sets the tag), else
// the crate's; and the commit built (GITHUB_SHA in Actions, else the checkout's HEAD), which the
// list page shows next to the name (GET /version) so an old binary on a server is plain to see.
// The web app is built in (webassets.rs): a new web/dist rebuilds the binary.
fn main() {
    println!("cargo:rerun-if-env-changed=IPPAI_VERSION");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    println!("cargo:rerun-if-changed=../../web/dist");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");
    let v = std::env::var("IPPAI_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
    println!("cargo:rustc-env=IPPAI_VERSION={v}");
    let sha = std::env::var("GITHUB_SHA").ok().filter(|s| !s.is_empty()).or_else(|| {
        let o = std::process::Command::new("git").args(["rev-parse", "HEAD"]).output().ok()?;
        o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
    });
    let commit: String = sha.unwrap_or_default().chars().take(7).collect();
    println!("cargo:rustc-env=IPPAI_COMMIT={commit}");
}
