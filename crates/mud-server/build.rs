//! Embeds the git commit into the binary so `mud-server --version` can say
//! which build is running (used by the prod deploy script's pre-switch check).
//!
//! `FIERYMUD_GIT_SHA` wins when set (the deploy script pins it); otherwise ask
//! git, otherwise report `unknown`.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=FIERYMUD_GIT_SHA");
    println!("cargo:rerun-if-changed=build.rs");

    let sha = std::env::var("FIERYMUD_GIT_SHA")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let out = Command::new("git")
                .args(["rev-parse", "--short=12", "HEAD"])
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=FIERYMUD_GIT_SHA={sha}");
}
