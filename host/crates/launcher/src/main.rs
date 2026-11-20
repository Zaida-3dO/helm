//! `helm-launcher.exe --profile <dir>`: the small always-on launcher (see the crate's docs).
//! A windows-subsystem program, so starting it at logon never opens a console.

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    helm_launcher::daemon::main()
}
