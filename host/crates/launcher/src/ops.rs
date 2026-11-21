//! The launcher's `api` ops, for Settings → Hub only. No agent tool reads or switches them: what
//! starts at logon is the person's own choice.
//!
//! | op | args | result |
//! |---|---|---|
//! | `launcher_get` | `{}` | [`LauncherView`]: `{enabled, startAtSignIn, running, registered, wakeReady}` |
//! | `launcher_set` | `{enabled?, startAtSignIn?}` (at least one) | the person's choice is saved and applied; answers the same view |

use crate::install::{self, LauncherView, Setup};
use crate::{hubline, runkey};
use helm_core::credentials::CredentialStore;
use helm_core::profile::Profile;
use helm_ipc::{ApiError, OpClass, OpCtx, OpRegistry};
use helm_platform::Platform;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;

pub fn register_ops(reg: &mut OpRegistry) {
    reg.blocking("launcher_get", OpClass::Blocking, get)
        .blocking("launcher_set", OpClass::Blocking, set);
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetArgs {
    enabled: Option<bool>,
    start_at_sign_in: Option<bool>,
}

fn get(ctx: &OpCtx, _args: Value) -> Result<Value, ApiError> {
    let profile = ctx.services.need::<Profile>()?;
    let platform = ctx.services.need::<Platform>()?;
    let mut v = with_setup(&profile, platform.keychain.clone(), install::view);
    // Pairing after Helm started turns the default on: catch up here rather than at the next start.
    if v.enabled != v.running && install::may_start_in_this_build() {
        v = with_setup(&profile, platform.keychain.clone(), install::apply);
    }
    to_value(&v)
}

fn set(ctx: &OpCtx, args: Value) -> Result<Value, ApiError> {
    let a: SetArgs = serde_json::from_value(args).map_err(|_| {
        ApiError::bad_args("Expected {enabled: true|false} or {startAtSignIn: true|false}.")
    })?;
    if a.enabled.is_none() && a.start_at_sign_in.is_none() {
        return Err(ApiError::bad_args(
            "Expected at least one of enabled or startAtSignIn.",
        ));
    }
    let profile = ctx.services.need::<Profile>()?;
    let platform = ctx.services.need::<Platform>()?;
    if !install::may_change(
        install::may_start_in_this_build(),
        is_default_profile(&profile),
    ) {
        return Err(ApiError::conflict(
            "This test build leaves the installed Helm's launcher alone. Run it with its own \
             profile to try the switch.",
        ));
    }
    install::save_choice(&profile.dir, a.enabled, a.start_at_sign_in)
        .map_err(|_| ApiError::internal("The launcher switch could not be saved."))?;
    to_value(&with_setup(
        &profile,
        platform.keychain.clone(),
        install::apply,
    ))
}

/// Bring the launcher in line at start-up, off the caller's thread (it may copy a file and wait
/// for an old launcher to stop).
pub fn start_at_launch(profile: Profile, keychain: Arc<dyn helm_platform::Keychain>) {
    std::thread::spawn(move || {
        let v = with_setup(&profile, keychain, install::apply);
        log::info!(
            "launcher: on {}, running {}, starts at logon {}",
            v.enabled,
            v.running,
            v.registered
        );
    });
}

fn with_setup<T>(
    profile: &Profile,
    keychain: Arc<dyn helm_platform::Keychain>,
    f: impl FnOnce(&Setup) -> T,
) -> T {
    let creds = CredentialStore::new(&profile.path("credentials"), &profile.hash, keychain);
    let paired = hubline::effective(&creds).usable();
    let helm_exe = std::env::current_exe().unwrap_or_default();
    let run_key = runkey::for_this_build();
    let is_default = is_default_profile(profile);
    let may_start = install::may_start_in_this_build();
    let setup = Setup {
        profile_dir: &profile.dir,
        profile_hash: &profile.hash,
        is_default_profile: is_default,
        paired,
        bundled: install::bundled_beside(&helm_exe),
        helm_exe,
        run_key: run_key.as_ref(),
        may_start,
        may_change: install::may_change(may_start, is_default),
    };
    f(&setup)
}

/// The profile is the platform default, not one chosen with `HELM_PROFILE_DIR`. Compared by
/// name only: the default folder is never opened to find out.
pub fn is_default_profile(profile: &Profile) -> bool {
    let norm = |p: &std::path::Path| {
        runkey::plain_path(p)
            .trim_end_matches(['\\', '/'])
            .to_lowercase()
    };
    helm_core::profile::default_profile_dir().is_ok_and(|d| norm(&d) == norm(&profile.dir))
}

fn to_value(v: &LauncherView) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}
