//! The app's side of the launcher: the files it hands over, its own copy of the program, the
//! `Run` value and the switch.
//!
//! Everything lives in the profile's `launcher` folder:
//!
//! | file | written by | what |
//! |---|---|---|
//! | `launcher.json` | Helm, at every start and every switch | `{helmExe, version, enabled}`: what to start, and whether the launcher should run at all |
//! | `choice.json` | Helm, when the person flips the switch | `{enabled}`; absent means the default (on while the PC is paired with a hub) |
//! | `status.json` | the launcher | `{hub, wake, at}`: whether its hub line is up and the hub has its wake details |
//! | `helm-launcher.exe` | Helm | its own copy of the program shipped beside `helm.exe`, so an update never finds it locked |
//!
//! Helm stops a running launcher (a named event) before it replaces that copy, and when the
//! switch goes off. Debug builds start the launcher only with `HELM_TEST_LAUNCHER=1`, and write
//! the real `Run` key only with `HELM_TEST_LAUNCHER_REGISTER=1` (see [`crate::runkey`]).

use crate::runkey::{self, RunKey};
use crate::sys;
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const DIR: &str = "launcher";
pub const EXE_NAME: &str = "helm-launcher.exe";
/// Debug builds only: let Helm start the launcher.
pub const TEST_START_ENV: &str = "HELM_TEST_LAUNCHER";
/// How long Helm waits for a launcher it asked to stop.
const STOP_WAIT: Duration = Duration::from_secs(5);

pub fn dir(profile_dir: &Path) -> PathBuf {
    profile_dir.join(DIR)
}

/// What Helm hands the launcher.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Handoff {
    pub helm_exe: String,
    pub version: String,
    pub enabled: bool,
}

pub fn read_handoff(profile_dir: &Path) -> Option<Handoff> {
    read_json(&dir(profile_dir).join("launcher.json"))
}

fn write_handoff(profile_dir: &Path, h: &Handoff) -> std::io::Result<()> {
    write_json(&dir(profile_dir).join("launcher.json"), h)
}

#[derive(Serialize, Deserialize)]
struct Choice {
    enabled: bool,
}

/// The person's own choice, if they made one.
pub fn choice(profile_dir: &Path) -> Option<bool> {
    read_json::<Choice>(&dir(profile_dir).join("choice.json")).map(|c| c.enabled)
}

pub fn save_choice(profile_dir: &Path, enabled: bool) -> std::io::Result<()> {
    write_json(&dir(profile_dir).join("choice.json"), &Choice { enabled })
}

/// On unless the person turned it off; with no choice made, on exactly while the PC is paired
/// with a hub (an unpaired install registers nothing).
pub fn effective_enabled(choice: Option<bool>, paired: bool) -> bool {
    choice.unwrap_or(paired)
}

/// What the launcher says about its line.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// `connected`, `offline`, `unsupported` (an older hub) or `off` (not paired).
    #[serde(default)]
    pub hub: String,
    /// The hub has taken this PC's wake details.
    #[serde(default)]
    pub wake: bool,
    #[serde(default)]
    pub at: i64,
}

pub fn read_status(profile_dir: &Path) -> Option<Status> {
    read_json(&dir(profile_dir).join("status.json"))
}

pub fn write_status(profile_dir: &Path, s: &Status) -> std::io::Result<()> {
    write_json(&dir(profile_dir).join("status.json"), s)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let bytes = helm_core::privfs::read_capped(path, 64 * 1024).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_json<T: Serialize>(path: &Path, v: &T) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        helm_core::privfs::create_private_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(v).map_err(std::io::Error::other)?;
    helm_core::privfs::write_atomic(path, &bytes)
}

/// What Settings → Hub shows (`launcher_get` and `launcher_set`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LauncherView {
    /// The switch as it is in force: the person's choice, else on while paired.
    pub enabled: bool,
    /// A launcher is running for this profile.
    pub running: bool,
    /// The `Run` value starts this profile's launcher at logon.
    pub registered: bool,
    /// The running launcher has given the hub what it needs to wake this PC.
    pub wake_ready: bool,
}

/// Everything [`apply`] and [`view`] need, gathered once by the caller.
pub struct Setup<'a> {
    pub profile_dir: &'a Path,
    pub profile_hash: &'a str,
    /// The profile is the platform default (`%LOCALAPPDATA%\Helm`), not one chosen with
    /// `HELM_PROFILE_DIR`.
    pub is_default_profile: bool,
    /// A hub connection is saved (or, in a debug build, the test one is set).
    pub paired: bool,
    /// The running Helm's own program file.
    pub helm_exe: PathBuf,
    /// The launcher program shipped beside Helm, when there is one.
    pub bundled: Option<PathBuf>,
    pub run_key: &'a dyn RunKey,
    /// Start the launcher when it is on and not running.
    pub may_start: bool,
}

impl Setup<'_> {
    fn copy(&self) -> PathBuf {
        dir(self.profile_dir).join(EXE_NAME)
    }
    fn value_name(&self) -> String {
        runkey::value_name(self.is_default_profile, self.profile_hash)
    }
    fn command(&self) -> String {
        runkey::command(&self.copy(), self.profile_dir)
    }
}

/// Whether a launcher may be started by this build: always in release builds, and in a debug
/// build only when asked, so test runs never leave one behind.
pub fn may_start_in_this_build() -> bool {
    !cfg!(debug_assertions) || std::env::var(TEST_START_ENV).ok().as_deref() == Some("1")
}

/// The launcher program beside the running Helm.
pub fn bundled_beside(helm_exe: &Path) -> Option<PathBuf> {
    helm_exe
        .parent()
        .map(|d| d.join(EXE_NAME))
        .filter(|p| p.is_file())
}

/// One apply at a time: at start and from the switch they may overlap.
fn apply_lock() -> &'static Mutex<()> {
    static L: Mutex<()> = Mutex::new(());
    &L
}

/// Bring the launcher in line with the switch: hand over the files, keep the profile's copy of the
/// program current, write or remove the `Run` value, and start or stop the launcher.
pub fn apply(s: &Setup) -> LauncherView {
    let _one = apply_lock().lock().unwrap_or_else(|p| p.into_inner());
    let enabled = effective_enabled(choice(s.profile_dir), s.paired);
    let handoff = Handoff {
        helm_exe: runkey::plain_path(&s.helm_exe),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        enabled,
    };
    if let Err(e) = write_handoff(s.profile_dir, &handoff) {
        log::warn!("launcher: its settings file was not written: {e}");
    }
    let name = s.value_name();
    if enabled {
        if let Some(src) = &s.bundled {
            if let Err(e) = refresh_copy(s, src) {
                log::warn!("launcher: its program was not copied into the profile: {e}");
            }
        }
        let copy = s.copy();
        if copy.is_file() {
            let command = s.command();
            if s.run_key.get(&name).as_deref() != Some(command.as_str()) {
                if let Err(e) = s.run_key.set(&name, &command) {
                    log::warn!("launcher: it could not be set to start at logon: {e}");
                }
            }
            if s.may_start && !running(s.profile_hash) {
                let dir_arg = runkey::plain_path(s.profile_dir);
                match sys::spawn_detached(
                    &copy,
                    &[OsStr::new("--profile"), OsStr::new(&dir_arg)],
                    &[],
                ) {
                    Ok(pid) => {
                        log::info!("launcher: started (pid {pid})");
                        // Let it take its instance mutex, so the view below says it runs.
                        let end = Instant::now() + Duration::from_secs(2);
                        while !running(s.profile_hash) && Instant::now() < end {
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                    Err(e) => log::warn!("launcher: it could not be started: {e}"),
                }
            }
        }
    } else {
        if let Err(e) = s.run_key.remove(&name) {
            log::warn!("launcher: its logon entry could not be removed: {e}");
        }
        stop(s.profile_hash);
    }
    view_unlocked(s)
}

/// The switch and the launcher as they are, changing nothing.
pub fn view(s: &Setup) -> LauncherView {
    view_unlocked(s)
}

fn view_unlocked(s: &Setup) -> LauncherView {
    let running = running(s.profile_hash);
    LauncherView {
        enabled: effective_enabled(choice(s.profile_dir), s.paired),
        running,
        registered: s.run_key.get(&s.value_name()).as_deref() == Some(s.command().as_str()),
        wake_ready: running && read_status(s.profile_dir).is_some_and(|st| st.wake),
    }
}

/// A launcher holds this profile's instance mutex.
pub fn running(profile_hash: &str) -> bool {
    sys::instance_exists(&sys::instance_name(profile_hash))
}

/// Ask this profile's launcher to stop, and wait (a little) until it has.
pub fn stop(profile_hash: &str) {
    if !sys::signal_stop(&sys::stop_name(profile_hash)) {
        return;
    }
    let end = Instant::now() + STOP_WAIT;
    while running(profile_hash) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Copy the shipped program into the profile when the copy is missing or differs, stopping a
/// launcher that runs from the old copy first.
fn refresh_copy(s: &Setup, src: &Path) -> std::io::Result<()> {
    let dest = s.copy();
    let new = std::fs::read(src)?;
    let same = std::fs::read(&dest)
        .map(|old| helm_core::sha256_hex(&old) == helm_core::sha256_hex(&new))
        .unwrap_or(false);
    if same {
        return Ok(());
    }
    stop(s.profile_hash);
    helm_core::privfs::create_private_dir_all(&dir(s.profile_dir))?;
    helm_core::privfs::write_atomic(&dest, &new)?;
    log::info!("launcher: its program in the profile was updated");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runkey::MemoryRunKey;

    fn setup<'a>(
        profile: &'a Path,
        key: &'a MemoryRunKey,
        bundled: Option<PathBuf>,
        paired: bool,
        is_default: bool,
    ) -> Setup<'a> {
        Setup {
            profile_dir: profile,
            profile_hash: "0123456789abcdef0123456789abcdef0123456789abcdef",
            is_default_profile: is_default,
            paired,
            helm_exe: profile.join("Program Files").join("helm.exe"),
            bundled,
            run_key: key,
            may_start: false,
        }
    }

    #[test]
    fn the_switch_registers_and_unregisters_through_the_run_key() {
        let root = tempfile::tempdir().unwrap();
        // A profile path with spaces, to prove the command line quotes it.
        let profile = root.path().join("My Profile");
        std::fs::create_dir_all(&profile).unwrap();
        let shipped = root.path().join(EXE_NAME);
        std::fs::write(&shipped, b"launcher v1").unwrap();
        let key = MemoryRunKey::default();

        // Unpaired with no choice: off, nothing registered, nothing copied.
        let v = apply(&setup(&profile, &key, Some(shipped.clone()), false, true));
        assert_eq!(v, LauncherView::default());
        assert!(key.values.lock().unwrap().is_empty());
        assert!(!dir(&profile).join(EXE_NAME).exists());
        assert!(!read_handoff(&profile).unwrap().enabled);

        // Paired: on by default, copied into the profile and registered with both paths quoted.
        let v = apply(&setup(&profile, &key, Some(shipped.clone()), true, true));
        assert!(
            v.enabled && v.registered && !v.running && !v.wake_ready,
            "{v:?}"
        );
        let copy = dir(&profile).join(EXE_NAME);
        assert_eq!(std::fs::read(&copy).unwrap(), b"launcher v1");
        let cmd = key.get(runkey::RUN_VALUE).unwrap();
        assert_eq!(
            cmd,
            format!(
                "\"{}\" --profile \"{}\"",
                runkey::plain_path(&copy),
                runkey::plain_path(&profile)
            )
        );
        assert!(cmd.contains("My Profile\\launcher") || cmd.contains("My Profile/launcher"));
        let h = read_handoff(&profile).unwrap();
        assert!(h.enabled);
        assert!(h.helm_exe.ends_with("helm.exe"), "{}", h.helm_exe);

        // A new build replaces the copy.
        std::fs::write(&shipped, b"launcher v2").unwrap();
        apply(&setup(&profile, &key, Some(shipped.clone()), true, true));
        assert_eq!(std::fs::read(&copy).unwrap(), b"launcher v2");

        // The person turns it off: the value goes, and the handoff says off.
        save_choice(&profile, false).unwrap();
        let v = apply(&setup(&profile, &key, Some(shipped.clone()), true, true));
        assert!(!v.enabled && !v.registered, "{v:?}");
        assert!(key.values.lock().unwrap().is_empty());
        assert!(!read_handoff(&profile).unwrap().enabled);

        // Turned on again while unpaired: the choice wins over the default.
        save_choice(&profile, true).unwrap();
        let v = apply(&setup(&profile, &key, Some(shipped.clone()), false, true));
        assert!(v.enabled && v.registered, "{v:?}");

        // Another profile gets its own value and never touches the default profile's.
        let other = root.path().join("Second Profile");
        std::fs::create_dir_all(&other).unwrap();
        let mut s = setup(&other, &key, Some(shipped), true, false);
        s.profile_hash = "fedcba9876543210fedcba9876543210";
        apply(&s);
        let values = key.values.lock().unwrap().clone();
        assert_eq!(
            values.keys().cloned().collect::<Vec<_>>(),
            vec![
                runkey::RUN_VALUE.to_owned(),
                format!("{} (fedcba98)", runkey::RUN_VALUE)
            ]
        );
        assert!(values[runkey::RUN_VALUE].contains("My Profile"));

        // The launcher's status reads back; wake counts only while it runs.
        write_status(
            &profile,
            &Status {
                hub: "connected".into(),
                wake: true,
                at: 1,
            },
        )
        .unwrap();
        assert!(read_status(&profile).unwrap().wake);
        assert!(!view(&setup(&profile, &key, None, true, true)).wake_ready);
    }
}
