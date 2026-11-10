//! A board project renamed in Helm: the new title is written back to the board.
//!
//! The rename itself is local and instant (`title_override`, see [`crate::store::update_project`]),
//! and the project is marked as owing the board its title (`title_pending`). This module sends it,
//! off the calling thread, with the standup client's own short deadline. When the board takes it,
//! the local override is dropped and the project reads as the board's again. When it cannot (no
//! connection, a refused token, a slow board) the local name stays and the flag stays set, the row
//! says so, and the next board sync sends it again.
//!
//! Only the title travels. Icon and colour stay local: the board has no field for them.

use crate::ops::emit_project;
use crate::store::{self, PendingTitle};
use crate::types::ProjectNode;
use helm_core::db::Database;
use helm_ipc::OpCtx;
use helm_standup::{Client, StandupError};
use std::sync::{Arc, Mutex};

/// The one way a title leaves Helm. Tests and checks register their own as a service.
pub trait TitleSender: Send + Sync {
    fn send(
        &self,
        client: &Client,
        board_project_id: &str,
        title: &str,
    ) -> Result<(), StandupError>;
}

/// The real sender: the standup client's title write.
pub struct ClientSender;

impl TitleSender for ClientSender {
    fn send(
        &self,
        client: &Client,
        board_project_id: &str,
        title: &str,
    ) -> Result<(), StandupError> {
        client.set_title(board_project_id, title)
    }
}

/// A replacement [`TitleSender`] (tests and checks).
#[derive(Clone)]
pub struct TitleSenderService(pub Arc<dyn TitleSender>);

/// One pass at a time: a rename made while a pass runs is picked up by the next one.
static RUNNING: Mutex<()> = Mutex::new(());

/// Send every owed title (or only `only`'s), on a thread of its own. Quiet when there is nothing to
/// send or no board connection.
pub fn push_in_background(ctx: &OpCtx, only: Option<String>) {
    let ctx = ctx.clone();
    let spawned = std::thread::Builder::new()
        .name("board-retitle".into())
        .spawn(move || push_pending(&ctx, only.as_deref()));
    if let Err(e) = spawned {
        log::warn!("board rename not sent: {e}");
    }
}

/// Send the owed titles now, on this thread. Returns how many the board took.
pub fn push_pending(ctx: &OpCtx, only: Option<&str>) -> usize {
    let Some(db) = ctx.services.get::<Database>() else {
        return 0;
    };
    let _one_at_a_time = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    match db.snapshot(|tx| store::pending_titles(tx, only)) {
        Ok(o) if !o.is_empty() => {}
        _ => return 0,
    }
    let Some(client) = client_for(ctx, &db) else {
        return 0;
    };
    let sender: Arc<dyn TitleSender> = match ctx.services.get::<TitleSenderService>() {
        Some(s) => s.0.clone(),
        None => Arc::new(ClientSender),
    };
    send_owed(&db, &client, sender.as_ref(), only, |node| {
        emit_project(ctx, node)
    })
}

/// Send each owed title through `sender`; a title the board took is recorded and handed to `done`.
/// Returns how many were taken. A title that could not be sent stays owed.
pub(crate) fn send_owed(
    db: &Database,
    client: &Client,
    sender: &dyn TitleSender,
    only: Option<&str>,
    mut done: impl FnMut(&ProjectNode),
) -> usize {
    let owed = db
        .snapshot(|tx| store::pending_titles(tx, only))
        .unwrap_or_default();
    let mut taken = 0;
    for p in owed {
        if !same_board(&p, client) {
            continue;
        }
        match sender.send(client, &p.board_project_id, &p.title) {
            Ok(()) => {
                let recorded = db.write(|tx| -> store::Res<_> {
                    store::title_sent(tx, &p.project_id, &p.title)?;
                    store::get_project(tx, &p.project_id)
                });
                match recorded {
                    Ok(node) => {
                        taken += 1;
                        done(&node);
                    }
                    Err(e) => log::warn!("board rename sent, but not recorded: {e:?}"),
                }
            }
            // The sentence names the cause without the token; the rename stays owed.
            Err(e) => log::info!("board rename waits for the next sync: {e}"),
        }
    }
    taken
}

fn client_for(ctx: &OpCtx, db: &Database) -> Option<Client> {
    let creds = crate::board::ctx_credentials(ctx).ok()?;
    crate::board::resolve(db, &creds).ok()
}

/// Whether the title belongs to the board the client talks to (the address may have changed).
fn same_board(p: &PendingTitle, client: &Client) -> bool {
    helm_standup::normalize_url(&p.board_url)
        .map(|u| u == client.base())
        .unwrap_or(false)
}
