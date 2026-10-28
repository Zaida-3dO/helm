//! The reads behind the Tasks view. Each is one fixed GET route with checked parameters; the
//! board's answer is handed back as JSON for the renderer to read, so a field Helm does not use
//! cannot break a read.
//!
//! | method | route |
//! |---|---|
//! | [`Client::board_page`] | `/api/board?column=…&level=exclude:0` (project rows never show as cards) |
//! | [`Client::item`] | `/api/items/<id>` (the slim item plus its latest checkpoint headline) |
//! | [`Client::item_detail`] | `/api/items/<id>/detail` |
//! | [`Client::history`] | `/api/items/<id>/history?full=true` |
//! | [`Client::artifacts`] | `/api/items/<id>/artifacts?full=true` |
//! | [`Client::loops`] / [`Client::loop_text`] | `/api/items/<id>/loops[/<loop id>]` |
//! | [`Client::fleet_page`] / [`Client::fleet`] | `/api/fleet` |
//! | [`Client::ready`] | `/api/ready` |
//!
//! Ids are 1 to 512 characters of letters, digits and `_.:-`, never `.` or `..`; cursors are at
//! most 4096 characters with no control characters; history and evidence cursors are all digits.
//! A value outside those forms is refused before anything is sent.

use crate::{encode, Client, StandupError};
use serde_json::{json, Value};

pub const COLUMNS: [&str; 4] = ["backlog", "in_progress", "waiting", "completed"];
pub const MAX_ID_CHARS: usize = 512;
pub const MAX_CURSOR_CHARS: usize = 4096;
/// Pages the complete fleet read drains (500 live assignments a page), so it ends however the
/// board answers.
pub const MAX_FLEET_PAGES: usize = 64;

fn invalid(m: &str) -> StandupError {
    StandupError::Invalid(m.to_owned())
}

/// A board item or loop id as it may appear in a path.
pub fn check_id(id: &str) -> Result<&str, StandupError> {
    let ok = !id.is_empty()
        && id.chars().count() <= MAX_ID_CHARS
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'));
    if ok {
        Ok(id)
    } else {
        Err(invalid("that is not an item id"))
    }
}

fn check_cursor(c: &str, digits: bool) -> Result<&str, StandupError> {
    let ok = !c.is_empty()
        && c.chars().count() <= MAX_CURSOR_CHARS
        && !c.chars().any(|ch| ch.is_control())
        && (!digits || c.chars().all(|ch| ch.is_ascii_digit()));
    if ok {
        Ok(c)
    } else {
        Err(invalid("that page marker is not usable"))
    }
}

fn limit(n: usize, max: usize) -> usize {
    n.clamp(1, max)
}

/// `&cursor=…` when there is one.
fn cursor_part(cursor: Option<&str>, digits: bool) -> Result<String, StandupError> {
    match cursor {
        Some(c) => Ok(format!("&cursor={}", encode(check_cursor(c, digits)?))),
        None => Ok(String::new()),
    }
}

/// Every live assignment, drained page by page. `complete` is false when the drain stopped at
/// [`MAX_FLEET_PAGES`] or the board repeated a cursor: the holders it shows may then be partial.
#[derive(Clone, Debug, PartialEq)]
pub struct FleetSnapshot {
    pub assignments: Vec<Value>,
    pub complete: bool,
}

impl Client {
    /// One page of one column. `project` limits the read to one project's whole subtree.
    pub fn board_page(
        &self,
        column: &str,
        project: Option<&str>,
        page_limit: usize,
        cursor: Option<&str>,
    ) -> Result<Value, StandupError> {
        if !COLUMNS.contains(&column) {
            return Err(invalid("that is not a board column"));
        }
        let mut path = format!(
            "/api/board?column={column}&level=exclude%3A0&limit={}",
            limit(page_limit, 200)
        );
        if let Some(p) = project {
            path.push_str(&format!("&project={}", encode(check_id(p)?)));
        }
        path.push_str(&cursor_part(cursor, false)?);
        self.get_json(&path)
    }

    pub fn item(&self, id: &str) -> Result<Value, StandupError> {
        self.get_json(&format!("/api/items/{}", encode(check_id(id)?)))
    }

    pub fn item_detail(
        &self,
        id: &str,
        history_limit: usize,
        artifact_limit: usize,
    ) -> Result<Value, StandupError> {
        self.get_json(&format!(
            "/api/items/{}/detail?historyLimit={}&artifactLimit={}",
            encode(check_id(id)?),
            limit(history_limit, 500),
            limit(artifact_limit, 500)
        ))
    }

    pub fn history(
        &self,
        id: &str,
        page_limit: usize,
        cursor: Option<&str>,
    ) -> Result<Value, StandupError> {
        self.get_json(&format!(
            "/api/items/{}/history?full=true&limit={}{}",
            encode(check_id(id)?),
            limit(page_limit, 200),
            cursor_part(cursor, true)?
        ))
    }

    pub fn artifacts(
        &self,
        id: &str,
        page_limit: usize,
        cursor: Option<&str>,
    ) -> Result<Value, StandupError> {
        self.get_json(&format!(
            "/api/items/{}/artifacts?full=true&limit={}{}",
            encode(check_id(id)?),
            limit(page_limit, 200),
            cursor_part(cursor, true)?
        ))
    }

    pub fn loops(
        &self,
        id: &str,
        page_limit: usize,
        cursor: Option<&str>,
        include_closed: bool,
    ) -> Result<Value, StandupError> {
        self.get_json(&format!(
            "/api/items/{}/loops?limit={}&includeClosed={}{}",
            encode(check_id(id)?),
            limit(page_limit, 200),
            include_closed,
            cursor_part(cursor, false)?
        ))
    }

    /// One loop's full text.
    pub fn loop_text(&self, id: &str, loop_id: &str) -> Result<Value, StandupError> {
        self.get_json(&format!(
            "/api/items/{}/loops/{}",
            encode(check_id(id)?),
            encode(check_id(loop_id)?)
        ))
    }

    pub fn fleet_page(
        &self,
        page_limit: usize,
        cursor: Option<&str>,
    ) -> Result<Value, StandupError> {
        self.get_json(&format!(
            "/api/fleet?limit={}{}",
            limit(page_limit, 500),
            cursor_part(cursor, false)?
        ))
    }

    /// The whole fleet: pages of 500 until the board has no more, a cursor repeats or
    /// [`MAX_FLEET_PAGES`] is reached.
    pub fn fleet(&self) -> Result<FleetSnapshot, StandupError> {
        let mut assignments = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..MAX_FLEET_PAGES {
            let page = self.fleet_page(500, cursor.as_deref())?;
            if let Some(list) = page.get("assignments").and_then(Value::as_array) {
                assignments.extend(list.iter().cloned());
            }
            match page.get("nextCursor").and_then(Value::as_str) {
                Some(c) if !c.is_empty() => {
                    if !seen.insert(c.to_owned()) {
                        return Ok(FleetSnapshot {
                            assignments,
                            complete: false,
                        });
                    }
                    cursor = Some(c.to_owned());
                }
                _ => {
                    return Ok(FleetSnapshot {
                        assignments,
                        complete: true,
                    })
                }
            }
        }
        Ok(FleetSnapshot {
            assignments,
            complete: false,
        })
    }

    /// Whether the board says it is ready. A board that answers "not ready" (503) is an answer, not
    /// an error: `{"ready": false}`.
    pub fn ready(&self) -> Result<Value, StandupError> {
        match self.get_json("/api/ready") {
            Err(StandupError::Status(503)) => Ok(json!({ "ready": false })),
            other => other,
        }
    }

    /// Board search with the Tasks view's rules: the text is 2 to 200 characters once
    /// whitespace is collapsed, and `limit` is 1 to 20.
    pub fn search_items(&self, query: &str, page_limit: usize) -> Result<Value, StandupError> {
        let flat: String = query
            .chars()
            .filter(|c| !c.is_control() || c.is_whitespace())
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let flat: String = flat.chars().take(200).collect();
        let flat = flat.trim();
        if flat.chars().count() < 2 {
            return Err(invalid("type at least two characters to search"));
        }
        self.search(flat, limit(page_limit, 20))
    }
}
