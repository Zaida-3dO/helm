//! Devices: which computer a chat runs on.
//!
//! Every install has one device identity, made the first time it is needed: a random 128-bit id
//! and a name people recognise. Each chat records the id of the device it runs on, so a chat keeps
//! its place when more than one device belongs to the household.
//!
//! | module | what |
//! |---|---|
//! | [`identity`] | the `device_self` table, the id, the default and edited names |
//! | `ops` | the `api` ops below |
//!
//! Ops (arguments and results are camelCase JSON; each change emits `device_changed`):
//!
//! | op | class | args |
//! |---|---|---|
//! | `device_get` | blocking | `{}` → `{uid, name}` |
//! | `device_rename` | maintenance | `{name}` (1 to 40 characters) → `{uid, name}` |

pub mod identity;
mod ops;

pub use identity::{self_name, self_uid, DeviceError};
pub use ops::register_ops;

pub fn register_migrations(reg: &mut helm_core::db::MigrationRegistry) {
    reg.initializer("devices-self", identity::init)
        .one_time("devices-make-self", identity::ensure_self);
}
