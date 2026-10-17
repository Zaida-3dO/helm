fn main() {
    // Declaring the app's one command makes Tauri generate an `allow-api` permission for it, so
    // only windows whose capability grants it can invoke `api` at all.
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(&["api"])),
    )
    .expect("tauri build script");
}
