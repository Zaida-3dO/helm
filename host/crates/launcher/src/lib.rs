//! The always-on launcher, and the app's side of it.
//!
//! Helm has no inbound port: the PC opens every connection to the hub. So when Helm is not
//! running, nothing on the PC can hear a message arrive. The launcher fills that gap. It is a
//! small Windows program, `helm-launcher.exe`, started at logon from the user's `Run` key, that
//! keeps one light outbound WebSocket to the hub's `/launcher` with the PC's relay token. It
//! starts Helm (hidden in the tray) when the hub asks, restarts Helm after a crash (never after an
//! orderly quit, and never while an update is writing the program), and tells the hub the PC's MAC
//! and LAN broadcast address so the hub can wake it. It has no window, no chats and no token but
//! the hub's, which it reads from the profile's credential store the way Helm does. When Windows
//! starts it from the `Run` key it also opens Helm, hidden, if the sign-in switch is on (it is by
//! default) and nothing is running; Helm itself starts the same program without that flag, so a
//! switch flipped inside Helm never opens a second Helm.
//!
//! | module | what |
//! |---|---|
//! | [`daemon`] | the launcher process: the watch on Helm, the hub line, the log |
//! | [`watch`] | `run.json`, and the restart rule [`watch::decide`] |
//! | [`frames`] | the launcher line's frames |
//! | [`wake`] | the MAC and directed broadcast of the adapter the line goes out on |
//! | [`hubline`] | the hub's addresses and token, read from the relay line's sealed envelope |
//! | [`install`] | Helm's side: the files it hands over, the copy of the program, the switch |
//! | [`runkey`] | the logon entry, behind a trait so tests never touch the registry |
//! | [`ops`] | `launcher_get` and `launcher_set`, for Settings → Hub only |
//! | [`sys`] | the Windows calls: processes, the instance mutex, the stop event, detached starts |
//!
//! The launcher switch is on by default for a PC paired with a hub and off for one that is not;
//! the sign-in switch is on by default everywhere, and switching it on by hand brings the
//! launcher with it. In both cases the person's own choice, once made, wins. Debug builds never
//! start the launcher or write the real `Run` key unless asked (`HELM_TEST_LAUNCHER=1`,
//! `HELM_TEST_LAUNCHER_REGISTER=1`).

pub mod daemon;
pub mod frames;
pub mod hubline;
pub mod install;
pub mod ops;
pub mod runkey;
pub mod sys;
pub mod wake;
pub mod watch;

pub use ops::{register_ops, start_at_launch};
