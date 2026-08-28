// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
  // studio#119: GUI-launched apps on macOS/Linux don't inherit PATH from the
  // user's shell rc files (only what launchd/the display manager hands them),
  // so exec-based kubeconfig auth plugins (e.g. `gke-gcloud-auth-plugin`,
  // installed via Homebrew-cask gcloud SDK) are invisible to the spawned
  // `oab-mcp` sidecar even when correctly installed. Must run before anything
  // else that could spawn a child process or need PATH-resolved binaries.
  let _ = fix_path_env::fix();
  app_lib::run();
}
