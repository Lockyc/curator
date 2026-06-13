use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    // Build stamp so a glance in About confirms the installed app matches a given commit.
    let sha = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&["status", "--porcelain"]).map(|s| !s.is_empty()).unwrap_or(false);
    let sha = if dirty { format!("{sha}-dirty") } else { sha };
    let date = Command::new("date")
        .arg("+%Y-%m-%d")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!("cargo:rustc-env=CURATOR_GIT_SHA={sha}");
    println!("cargo:rustc-env=CURATOR_BUILD_DATE={date}");
    // Re-stamp after any git ref update (commit/checkout) — logs/HEAD changes every time.
    println!("cargo:rerun-if-changed=../.git/logs/HEAD");

    tauri_build::build()
}
