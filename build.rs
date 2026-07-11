//! Embeds `assets/icon.ico` (the AW badge — see `scripts/generate-icon.ps1`) as the
//! compiled .exe's Windows resource icon: what Explorer, the taskbar, and Alt-Tab show,
//! as opposed to the tray icon or in-app window icons (set at runtime in `theme.rs`).

fn main() {
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.compile().expect("failed to embed the Windows .exe icon resource");
    }
}
