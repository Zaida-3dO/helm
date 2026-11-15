//! The project line of a chat's system prompt, and the switch for the startup Stop hook.
//!
//! Every chat is told where it sits: the board it is shown under (with its description) and the
//! folder it runs in. A chat on no board is told its folder project instead. A part that is
//! missing is left out, and a chat Helm cannot find gets no line at all.
//!
//! The same contributor sets [`STARTUP_GATE_ENV`] on the launch of a chat that was told its
//! project's startup and has not reported it, which makes the worker ask the host before every
//! turn ends ([`crate::startup::owes_startup`]). Every other launch is unchanged.

use crate::startup;
use helm_core::db::Database;
use helm_workers::contrib::{LaunchContributor, LaunchCtx, LaunchExtras};
use rusqlite::{OptionalExtension, Transaction};
use std::sync::Arc;

/// The launch variable that switches the worker's startup check on.
pub const STARTUP_GATE_ENV: &str = "HELM_STARTUP_GATE";

pub struct ProjectLaunch {
    pub db: Arc<Database>,
}

impl LaunchContributor for ProjectLaunch {
    fn contribute(&self, ctx: &LaunchCtx<'_>) -> LaunchExtras {
        let read = self.db.snapshot(|tx| {
            Ok::<_, rusqlite::Error>((
                project_line(tx, ctx.chat_id, ctx.cwd)?,
                startup::owes_startup(tx, ctx.chat_id)?,
            ))
        });
        let (line, owes) = match read {
            Ok(v) => v,
            Err(e) => {
                log::warn!("the project line could not be read: {e}");
                (None, false)
            }
        };
        let mut e = LaunchExtras {
            system_prompt: line.into_iter().collect(),
            ..Default::default()
        };
        if owes {
            e.env.push((STARTUP_GATE_ENV.to_owned(), "1".to_owned()));
        }
        e
    }
}

/// What a project is called and what it says about itself.
struct Named {
    name: String,
    description: Option<String>,
}

fn named(tx: &Transaction<'_>, project_id: &str) -> rusqlite::Result<Option<Named>> {
    tx.query_row(
        "SELECT COALESCE(l.title_override, b.title, p.name), l.description
         FROM projects p
         LEFT JOIN project_local l ON l.project_id = p.id
         LEFT JOIN board_projects b ON b.project_id = p.id
         WHERE p.id = ?1",
        [project_id],
        |r| {
            Ok(Named {
                name: r.get(0)?,
                description: r
                    .get::<_, Option<String>>(1)?
                    .filter(|d| !d.trim().is_empty()),
            })
        },
    )
    .optional()
}

/// The line for a chat: its board and that board's description, else its folder project; and the
/// folder it runs in. `None` for a chat Helm does not know.
pub fn project_line(
    tx: &Transaction<'_>,
    chat_id: &str,
    cwd: &str,
) -> rusqlite::Result<Option<String>> {
    let folder: Option<String> = tx
        .query_row(
            "SELECT project_id FROM chats WHERE id = ?1",
            [chat_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(folder) = folder else {
        return Ok(None);
    };
    let folder = named(tx, &folder)?;
    let board = match crate::boards::chat_board(tx, chat_id)? {
        Some(b) => named(tx, &b)?,
        None => None,
    };
    let (kind, project) = match (board, folder) {
        (Some(b), f) => {
            let description = b.description.clone().or(f.and_then(|f| f.description));
            ("the board", Named { description, ..b })
        }
        (None, Some(f)) => ("the project", f),
        (None, None) => return Ok(None),
    };
    let mut line = format!("This chat is on {kind} \"{}\"", project.name);
    if let Some(d) = &project.description {
        line.push_str(&format!(" ({d})"));
    }
    if !cwd.trim().is_empty() {
        line.push_str(&format!(" and runs in {}", cwd.trim()));
    }
    line.push('.');
    Ok(Some(line))
}
