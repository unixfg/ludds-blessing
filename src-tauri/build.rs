fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        tauri_build::build();
        return;
    }

    // `tauri-winres` canonicalizes this path and escapes an apostrophe as `\'`.
    // rc.exe treats that backslash literally, so a workspace such as
    // `Ludd's Blessing` cannot embed its icon directly. Compile from a
    // process-unique temporary copy, then remove it after the resource exists.
    println!("cargo:rerun-if-changed=icons/icon.ico");
    let temporary_icon =
        std::env::temp_dir().join(format!("ludds-blessing-winres-{}.ico", std::process::id()));
    std::fs::copy("icons/icon.ico", &temporary_icon)
        .expect("failed to stage the Windows application icon");

    let windows = tauri_build::WindowsAttributes::new().window_icon_path(&temporary_icon);
    let attributes = tauri_build::Attributes::new().windows_attributes(windows);
    let result = tauri_build::try_build(attributes);
    let _ = std::fs::remove_file(temporary_icon);
    result.expect("failed to run Tauri build script");
}
