//! What the targets share.

/// WGSL a translator wrote: naga parses and validates it (as the page's WebGPU will).
pub fn valid_wgsl(code: &str) {
    let m = naga::front::wgsl::parse_str(code).unwrap_or_else(|e| panic!("invalid WGSL: {}\n{code}", e.emit_to_string(code)));
    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(&m)
        .unwrap_or_else(|e| panic!("WGSL naga does not validate: {e:?}\n{code}"));
}
