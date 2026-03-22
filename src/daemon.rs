use std::path::PathBuf;
use std::process::{Command, Stdio};

const DEFAULT_CDP_PORT: u16 = 9222;

/// Find Chromium/Chrome binary on the system.
fn find_chrome() -> Result<PathBuf, String> {
    let candidates = [
        // macOS
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        // Linux common paths
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
    ];

    for path in &candidates {
        if std::path::Path::new(path).exists() {
            return Ok(PathBuf::from(path));
        }
    }

    // Try PATH
    for name in &["chromium", "chromium-browser", "google-chrome", "google-chrome-stable"] {
        if let Ok(output) = Command::new("which").arg(name).output() {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() {
                    return Ok(PathBuf::from(path));
                }
            }
        }
    }

    Err("No Chromium/Chrome browser found. Install Chromium or set chrome_executable in config.".into())
}

/// Pechincha's own browser profile directory — completely separate from your personal browser.
fn profile_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pechincha")
        .join("browser-profile")
}

fn pid_file() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pechincha")
        .join("daemon.pid")
}

/// Check if the pechincha daemon is running on its port.
/// Only returns true if it's OUR daemon (checks PID file), not your personal browser.
pub fn is_running() -> bool {
    // First check PID file exists and process is alive
    if let Ok(pid_str) = std::fs::read_to_string(pid_file()) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            #[cfg(unix)]
            {
                // kill with signal 0 checks if process exists without killing it
                let alive = unsafe { libc::kill(pid, 0) } == 0;
                if alive {
                    return true;
                }
            }
        }
    }

    false
}

/// Check if ANY browser has CDP listening on the given port (including personal browser).
pub fn is_cdp_available(port: u16) -> bool {
    std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok()
}

/// Start the pechincha browser daemon.
///
/// This launches a SEPARATE browser instance with its own profile directory.
/// It will NOT interfere with your personal browser.
///
/// - `headless`: Run without a visible window (use after first login)
/// - Returns the CDP port number
pub fn start(headless: bool) -> Result<u16, String> {
    // Check if our daemon is already running
    if is_running() {
        println!("Daemon already running on port {DEFAULT_CDP_PORT}.");
        return Ok(DEFAULT_CDP_PORT);
    }

    // Check if the port is taken by something else (e.g., personal browser)
    if is_cdp_available(DEFAULT_CDP_PORT) {
        return Err(format!(
            "Port {DEFAULT_CDP_PORT} is already in use by another process.\n\
             If that's your personal browser with --remote-debugging-port, pechincha can use it directly.\n\
             Otherwise, stop what's using port {DEFAULT_CDP_PORT} first."
        ));
    }

    let chrome = find_chrome()?;
    let profile = profile_dir();

    // Clean up stale lock files from previous runs
    let lock_file = profile.join("SingletonLock");
    if lock_file.exists() {
        let _ = std::fs::remove_file(&lock_file);
    }

    // Create profile directory
    std::fs::create_dir_all(&profile)
        .map_err(|e| format!("Failed to create profile dir: {e}"))?;

    let mut args = vec![
        format!("--remote-debugging-port={DEFAULT_CDP_PORT}"),
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--disable-background-networking".to_string(),
        "--disable-sync".to_string(),
        "--disable-translate".to_string(),
        "--metrics-recording-only".to_string(),
    ];

    if headless {
        args.push("--headless=new".to_string());
    } else {
        // Open Shopee so user can log in immediately
        args.push("https://shopee.com.br".to_string());
    }

    // Launch as detached process
    let child = Command::new(&chrome)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Failed to launch {}: {e}", chrome.display()))?;

    // Save PID for clean shutdown
    let pid = child.id();
    let _ = std::fs::write(pid_file(), pid.to_string());

    // Wait for CDP port to become available
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(15);

    while start.elapsed() < timeout {
        if is_cdp_available(DEFAULT_CDP_PORT) {
            let mode = if headless { "headless" } else { "visible" };
            println!("Pechincha browser daemon started ({mode}) on port {DEFAULT_CDP_PORT}.");
            println!("Profile: {}", profile.display());
            println!("PID: {pid}");
            println!();
            if headless {
                println!("Searches will automatically use this daemon for Shopee/AliExpress.");
            } else {
                println!("Log into Shopee/AliExpress in the browser window, then:");
                println!("  pechincha daemon stop");
                println!("  pechincha daemon start --headless");
            }
            return Ok(DEFAULT_CDP_PORT);
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    // If we get here, browser didn't start properly — clean up
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    let _ = std::fs::remove_file(pid_file());

    Err("Browser started but CDP port not responding after 15s. Check if Chromium is installed correctly.".into())
}

/// Stop the pechincha browser daemon.
/// Only kills OUR daemon process — never touches your personal browser.
pub fn stop() -> Result<(), String> {
    let pid_path = pid_file();

    if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            #[cfg(unix)]
            {
                // Check it's actually alive before killing
                let alive = unsafe { libc::kill(pid, 0) } == 0;
                if alive {
                    // SIGTERM for graceful shutdown
                    unsafe { libc::kill(pid, libc::SIGTERM); }

                    // Wait up to 3s for graceful exit, then force kill
                    for _ in 0..6 {
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        if unsafe { libc::kill(pid, 0) } != 0 {
                            break;
                        }
                    }

                    // Force kill if still alive
                    if unsafe { libc::kill(pid, 0) } == 0 {
                        unsafe { libc::kill(pid, libc::SIGKILL); }
                    }
                }
            }

            // Clean up
            let _ = std::fs::remove_file(&pid_path);
            let lock_file = profile_dir().join("SingletonLock");
            let _ = std::fs::remove_file(&lock_file);

            println!("Daemon stopped (PID {pid}).");
            return Ok(());
        }
    }

    let _ = std::fs::remove_file(&pid_path);
    let lock_file = profile_dir().join("SingletonLock");
    let _ = std::fs::remove_file(&lock_file);

    println!("No daemon running.");
    Ok(())
}

/// Get daemon status.
pub fn status() -> String {
    if is_running() {
        let pid = std::fs::read_to_string(pid_file())
            .unwrap_or_else(|_| "unknown".to_string());
        format!("Pechincha daemon running on port {DEFAULT_CDP_PORT} (PID: {})", pid.trim())
    } else if is_cdp_available(DEFAULT_CDP_PORT) {
        format!("External browser detected on port {DEFAULT_CDP_PORT} (not managed by pechincha)")
    } else {
        "Not running".to_string()
    }
}
