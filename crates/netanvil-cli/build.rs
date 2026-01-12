use std::process::Command;

fn main() {
    // Embed git revision so startup logs identify exactly which build is running.
    let git_hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());

    let git_dirty = Command::new("git")
        .args(["diff", "--quiet", "HEAD"])
        .status()
        .map(|s| if s.success() { "" } else { "-dirty" })
        .unwrap_or("");

    println!("cargo:rustc-env=NETANVIL_GIT_HASH={git_hash}{git_dirty}");
    // Re-run if HEAD changes (new commit) or working tree changes.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
}
