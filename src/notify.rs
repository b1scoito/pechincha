//! Desktop notifications — sends alerts via OS-native mechanisms.

use tracing::debug;

/// Send a desktop notification.
pub fn send(title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            r#"display notification "{}" with title "{}""#,
            body.replace('"', "\\\""),
            title.replace('"', "\\\"")
        );
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .output();
        debug!(title = title, "macOS notification sent");
    }

    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("notify-send")
            .args([title, body])
            .output();
        debug!(title = title, "Linux notification sent");
    }

    // Always print to stderr as fallback
    eprintln!("  🔔 {title} — {body}");
}
