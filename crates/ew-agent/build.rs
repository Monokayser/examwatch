// Embeds a manifest: Common Controls v6 (modern buttons/edits), per-monitor
// DPI awareness v2 (crisp text on high-DPI screens), asInvoker (no UAC prompt).
fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        use embed_manifest::manifest::DpiAwareness;
        embed_manifest::embed_manifest(
            embed_manifest::new_manifest("ExamWatch.Agent").dpi_awareness(DpiAwareness::PerMonitorV2Only),
        )
        .expect("unable to embed manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
