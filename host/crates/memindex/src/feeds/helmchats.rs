//! This PC's Helm chats: one doc per chat, a chunk per turn. A chat belongs to its profile's
//! person and is shown to the people of the profiles it is shared with; a profile the hub does not
//! know owns nothing, and a share to one is dropped, so that chat stays private.

use super::{Audience, Built, Entry, Env, Feed, Listing};
use crate::sources::chat_turns::{build_chat_doc, MsgRole, MsgRow};
use rusqlite::params;
use std::sync::Arc;

pub struct HelmChatsFeed {
    env: Arc<Env>,
}

impl HelmChatsFeed {
    pub fn new(env: Arc<Env>) -> Self {
        HelmChatsFeed { env }
    }
}

impl Feed for HelmChatsFeed {
    fn list(&self) -> Result<Listing, String> {
        let entries = self
            .env
            .db
            .snapshot(|tx| {
                let mut st = tx.prepare(
                    "SELECT c.id, c.updated_at, c.title, c.profile_id,
                            COALESCE((SELECT group_concat(profile_id) FROM
                                (SELECT profile_id FROM chat_shares WHERE chat_id = c.id
                                 ORDER BY profile_id)), '')
                     FROM chats c
                     WHERE EXISTS (SELECT 1 FROM chat_search_source s WHERE s.chat_id = c.id)
                     ORDER BY c.updated_at DESC, c.id",
                )?;
                let rows = st.query_map([], |r| {
                    Ok(Entry {
                        key: r.get(0)?,
                        stamp: format!(
                            "{}|{}|{}|{}",
                            r.get::<_, i64>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, String>(4)?
                        ),
                    })
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .map_err(|e| e.to_string())?;
        Ok(Listing {
            entries,
            complete: true,
            note: None,
        })
    }

    fn build(&self, key: &str) -> Result<Built, String> {
        type Head = (String, String, String);
        let loaded: Option<(Head, Vec<MsgRow>, Vec<String>)> = self
            .env
            .db
            .snapshot(|tx| {
                let head: Option<Head> = tx
                    .query_row(
                        "SELECT c.title, COALESCE(p.name, ''), c.profile_id
                         FROM chats c LEFT JOIN projects p ON p.id = c.project_id WHERE c.id = ?1",
                        [key],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .ok();
                let Some(head) = head else {
                    return Ok(None);
                };
                let mut st = tx.prepare(
                    "SELECT message_id, role, text, timestamp FROM chat_search_source
                     WHERE chat_id = ?1 ORDER BY timestamp, message_id",
                )?;
                let rows = st
                    .query_map(params![key], |r| {
                        let role: String = r.get(1)?;
                        Ok(MsgRow {
                            id: r.get(0)?,
                            role: if role == "user" {
                                MsgRole::User
                            } else {
                                MsgRole::Assistant
                            },
                            text: r.get(2)?,
                            at_ms: r.get(3)?,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let shares = tx
                    .prepare("SELECT profile_id FROM chat_shares WHERE chat_id = ?1")?
                    .query_map([key], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok::<_, rusqlite::Error>(Some((head, rows, shares)))
            })
            .map_err(|e| e.to_string())?;
        let Some(((title, project, profile_id), rows, shares)) = loaded else {
            return Ok(Built::Skip("the chat is gone".into()));
        };
        let Some(profile) = self.env.profile(&profile_id) else {
            return Ok(Built::Skip("the chat's profile is not set up here".into()));
        };
        let owner = match self.env.owner_of(&profile) {
            Ok(o) => o,
            Err(why) => return Ok(Built::Skip(why)),
        };
        let mut readers: Vec<String> = shares
            .iter()
            .filter_map(|id| self.env.profile(id))
            .filter_map(|p| p.hub_person_id.filter(|h| !h.is_empty()))
            .collect();
        readers.sort();
        readers.dedup();
        let audience = Audience {
            owner,
            scope: if readers.is_empty() { "me" } else { "some" },
            readers,
        };
        let Some(doc) = build_chat_doc(key, &title, &project, &rows) else {
            return Ok(Built::Skip("nothing was asked".into()));
        };
        Ok(super::finish(
            doc,
            &audience,
            "conversation",
            &format!("helm://chats/{key}"),
        ))
    }
}
