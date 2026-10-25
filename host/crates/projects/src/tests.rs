//! SB1 (projects), SB2 (tree), SB5 (paging), SB6 (organisation) store tests.

use crate::schema::tests::temp_db;
use crate::store::{self, Refusal};
use crate::types::*;
use helm_chats::types::ChatStatus;
use helm_core::db::Database;
use rusqlite::params;

fn folder(db: &Database, name: &str, path: Option<&str>) -> String {
    db.write(|tx| {
        store::create_project(
            tx,
            &ProjectCreateArgs {
                name: name.into(),
                path: path.map(Into::into),
                parent_id: None,
            },
            1,
        )
    })
    .unwrap()
}

fn chat_in(db: &Database, cwd: &str, now: i64) -> String {
    db.write(|tx| helm_chats::store::create_chat(tx, cwd, None, None, now))
        .unwrap()
}

fn project_of(db: &Database, chat: &str) -> String {
    db.snapshot(|tx| {
        tx.query_row("SELECT project_id FROM chats WHERE id = ?1", [chat], |r| {
            r.get(0)
        })
    })
    .unwrap()
}

fn node(db: &Database, id: &str) -> ProjectNode {
    db.snapshot(|tx| store::get_project(tx, id)).unwrap()
}

fn update(db: &Database, a: ProjectUpdateArgs) -> Result<i64, Refusal> {
    db.write(|tx| store::update_project(tx, &a))
}

fn upd(id: &str, db: &Database) -> ProjectUpdateArgs {
    ProjectUpdateArgs {
        project_id: id.into(),
        revision: node(db, id).revision,
        ..Default::default()
    }
}

fn set_parent(db: &Database, id: &str, parent: Option<&str>) -> Result<i64, Refusal> {
    let mut a = upd(id, db);
    a.parent_id = Some(parent.map(Into::into));
    update(db, a)
}

fn archived(db: &Database, chat: &str) -> bool {
    db.snapshot(|tx| {
        tx.query_row("SELECT archived FROM chats WHERE id = ?1", [chat], |r| {
            r.get::<_, i64>(0)
        })
    })
    .unwrap()
        != 0
}

fn status(db: &Database, chat: &str, s: &str) {
    db.write(|tx| tx.execute("UPDATE chats SET status = ?2 WHERE id = ?1", [chat, s]))
        .unwrap();
}

// ---- SB1 Projects --------------------------------------------------------------------------

#[test]
fn names_are_1_to_120_bytes_without_separators_or_controls() {
    assert_eq!(store::validate_name("  Web app ").unwrap(), "Web app");
    assert!(store::validate_name(&"é".repeat(60)).is_ok()); // 120 bytes
    for bad in [
        "", "   ", ".", "..", "a/b", "a\\b", "a:b", "a\u{7}b", "a\nb",
    ] {
        assert!(
            store::validate_name(bad).is_err(),
            "{bad:?} should be refused"
        );
    }
    assert!(store::validate_name(&"e".repeat(120)).is_ok());
    assert!(store::validate_name(&format!("{}e", "é".repeat(60))).is_err()); // 121 bytes
}

#[test]
fn there_is_no_delete_op_for_projects() {
    let mut reg = helm_ipc::OpRegistry::new();
    crate::register_ops(&mut reg);
    assert!(reg.contains("project_archive"));
    let names: Vec<&str> = reg.names().collect();
    assert!(
        !names
            .iter()
            .any(|n| n.starts_with("project_") && n.contains("delete")),
        "{names:?}"
    );
}

#[test]
fn changing_a_folder_needs_every_chat_idle() {
    let (_d, db) = temp_db();
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let a_path = a.path().to_string_lossy().into_owned();
    let chat = chat_in(&db, &a_path, 1);
    let p = project_of(&db, &chat);
    status(&db, &chat, "running");
    let mut args = upd(&p, &db);
    args.path = Some(Some(b.path().to_string_lossy().into_owned()));
    assert!(matches!(
        update(&db, args.clone()),
        Err(Refusal::Refused(_))
    ));
    assert_eq!(node(&db, &p).path.as_deref(), Some(a_path.as_str()));
    status(&db, &chat, "idle");
    update(&db, args).unwrap();
    assert_eq!(
        node(&db, &p).path,
        Some(b.path().to_string_lossy().into_owned())
    );
}

#[test]
fn archive_restores_exactly_the_chats_it_archived() {
    let (_d, db) = temp_db();
    let before = chat_in(&db, "C:/w/app", 1);
    let c2 = chat_in(&db, "C:/w/app", 2);
    let c3 = chat_in(&db, "C:/w/app", 3);
    let p = project_of(&db, &c2);
    db.write(|tx| tx.execute("UPDATE chats SET archived = 1 WHERE id = ?1", [&before]))
        .unwrap();
    let ids = db.write(|tx| store::archive_project(tx, &p)).unwrap();
    assert_eq!(ids.len(), 2);
    assert!(archived(&db, &c2) && archived(&db, &c3));
    assert!(node(&db, &p).archived);
    db.write(|tx| store::restore_project(tx, &p)).unwrap();
    assert!(!archived(&db, &c2) && !archived(&db, &c3));
    assert!(
        archived(&db, &before),
        "a chat archived beforehand stays archived"
    );
    assert!(!node(&db, &p).archived);
}

#[test]
fn archive_refuses_while_a_chat_is_busy() {
    let (_d, db) = temp_db();
    let c = chat_in(&db, "C:/w/app", 1);
    let p = project_of(&db, &c);
    status(&db, &c, "starting");
    assert!(matches!(
        db.write(|tx| store::archive_project(tx, &p)),
        Err(Refusal::Refused(_))
    ));
    assert!(!archived(&db, &c));
}

#[test]
fn an_archived_project_refuses_new_chats_and_chat_restores() {
    let (_d, db) = temp_db();
    let c = chat_in(&db, "C:/w/app", 1);
    let p = project_of(&db, &c);
    db.write(|tx| store::archive_project(tx, &p)).unwrap();
    let err = db
        .write(|tx| helm_chats::store::create_chat(tx, "C:/w/app", None, None, 2))
        .unwrap_err();
    assert!(
        err.to_string().contains("Restore this project first"),
        "{err}"
    );
    let err = db
        .write(|tx| tx.execute("UPDATE chats SET archived = 0 WHERE id = ?1", [&c]))
        .unwrap_err();
    assert!(
        err.to_string().contains("Restore this project first"),
        "{err}"
    );
    // Grouping a chat under it is refused too.
    let other = chat_in(&db, "C:/w/other", 3);
    let r = db.write(|tx| {
        store::organize(
            tx,
            &ChatOrganizeArgs {
                chat_id: other.clone(),
                group_project_id: Some(p.clone()),
                ..Default::default()
            },
        )
    });
    assert!(matches!(r, Err(Refusal::Refused(_))));
}

#[test]
fn update_is_compare_and_set_on_revision() {
    let (_d, db) = temp_db();
    let p = folder(&db, "Docs", None);
    let mut a = upd(&p, &db);
    a.name = Some("Docs 2".into());
    update(&db, a.clone()).unwrap();
    a.name = Some("Docs 3".into());
    assert!(matches!(update(&db, a), Err(Refusal::Conflict(_))));
    assert_eq!(node(&db, &p).name, "Docs 2");
}

// ---- SB2 Project tree ----------------------------------------------------------------------

/// A board project as the sync writes it (the mirror lands in the next PR; this is its shape).
fn board_project(db: &Database, board_id: &str, title: &str) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    db.write(|tx| {
        tx.execute(
            "INSERT INTO projects(id, kind, name, created_at) VALUES (?1, 'board', ?2, 1)",
            params![id, title],
        )?;
        tx.execute(
            "INSERT INTO board_projects(project_id, board_url, board_project_id, title, synced_at)
             VALUES (?1, 'https://board.test', ?2, ?3, 1)",
            params![id, board_id, title],
        )
    })
    .unwrap();
    id
}

#[test]
fn three_levels_nest_locally_and_depth_cycles_and_self_are_refused() {
    let (_d, db) = temp_db();
    let a = board_project(&db, "b-a", "Alpha");
    let b = board_project(&db, "b-b", "Beta");
    let c = folder(&db, "Gamma", None);
    let d = folder(&db, "Delta", None);
    assert!(
        node(&db, &a).parent_id.is_none(),
        "board projects start at the top level"
    );
    set_parent(&db, &b, Some(&a)).unwrap();
    set_parent(&db, &c, Some(&b)).unwrap();
    assert_eq!(node(&db, &c).parent_id.as_deref(), Some(b.as_str()));
    assert_eq!(node(&db, &b).parent_source, Some(ParentSource::Local));
    // Depth index 3 is refused, with a reason.
    match set_parent(&db, &d, Some(&c)) {
        Err(Refusal::BadArgs(m)) => assert!(m.contains("three levels"), "{m}"),
        r => panic!("{r:?}"),
    }
    // A subtree counts: moving A (with B and C under it) under D would make C depth 3.
    assert!(matches!(
        set_parent(&db, &a, Some(&d)),
        Err(Refusal::BadArgs(_))
    ));
    // Cycles and self.
    assert!(matches!(
        set_parent(&db, &a, Some(&c)),
        Err(Refusal::BadArgs(_))
    ));
    assert!(matches!(
        set_parent(&db, &a, Some(&a)),
        Err(Refusal::BadArgs(_))
    ));
    // Back to the top level.
    set_parent(&db, &c, None).unwrap();
    assert!(node(&db, &c).parent_id.is_none());
}

#[test]
fn a_board_rename_is_a_local_override_and_the_board_title_is_kept() {
    let (_d, db) = temp_db();
    let a = board_project(&db, "b-a", "Alpha");
    let mut args = upd(&a, &db);
    args.name = Some("My alpha".into());
    args.icon = Some(Some("🚀".into()));
    update(&db, args).unwrap();
    let n = node(&db, &a);
    assert_eq!(n.name, "My alpha");
    let b = n.board.unwrap();
    assert_eq!(b.title, "Alpha");
    assert!(b.edited_locally);
    // Renaming back to the board title drops the override.
    let mut args = upd(&a, &db);
    args.name = Some("Alpha".into());
    args.icon = Some(None);
    update(&db, args).unwrap();
    assert!(!node(&db, &a).board.unwrap().edited_locally);
}

#[test]
fn project_reorder_rewrites_positions_and_a_no_op_writes_nothing() {
    let (_d, db) = temp_db();
    let a = folder(&db, "A", None);
    let b = folder(&db, "B", None);
    let c = folder(&db, "C", None);
    let order = |db: &Database| -> Vec<String> {
        db.snapshot(|tx| store::list_projects(tx, false))
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect()
    };
    // New projects go to the top of the unpinned group.
    assert_eq!(order(&db), ["C", "B", "A"]);
    let mut m = upd(&a, &db);
    m.before_project_id = Some(Some(c.clone()));
    update(&db, m).unwrap();
    assert_eq!(order(&db), ["A", "C", "B"]);
    // Placing before its current successor changes nothing.
    let pos_before = node(&db, &c).position;
    let mut m = upd(&a, &db);
    m.before_project_id = Some(Some(c.clone()));
    update(&db, m).unwrap();
    assert_eq!(node(&db, &c).position, pos_before);
    // Pinned projects sort first; moving across pin groups is refused.
    let mut p = upd(&b, &db);
    p.pinned = Some(true);
    update(&db, p).unwrap();
    assert_eq!(order(&db)[0], "B");
    let mut m = upd(&a, &db);
    m.before_project_id = Some(Some(b.clone()));
    assert!(update(&db, m).is_err());
}

#[test]
fn colours_and_icons_are_validated() {
    assert_eq!(store::validate_colour("#AbCdEf").unwrap(), "#abcdef");
    assert_eq!(store::validate_colour("teal").unwrap(), "teal");
    assert!(store::validate_colour("#abc").is_err());
    assert!(store::validate_colour("mauve").is_err());
    assert!(store::validate_icon("🚀").is_ok());
    assert!(store::validate_icon("lib:rocket").is_ok());
    assert!(store::validate_icon("abc").is_err());
    assert!(store::validate_icon("lib:").is_err());
}

// ---- SB5 paging ----------------------------------------------------------------------------

#[test]
fn sidebar_pages_hold_500_rows_with_a_cursor() {
    let (_d, db) = temp_db();
    db.write(|tx| {
        for i in 0..1201 {
            helm_chats::store::create_chat(tx, "C:/w/app", None, None, i)?;
        }
        Ok::<_, rusqlite::Error>(())
    })
    .unwrap();
    let page = |cursor: Option<String>| {
        db.snapshot(|tx| {
            store::sidebar_page(
                tx,
                &SidebarPageArgs {
                    view: SidebarView::Projects,
                    group_id: None,
                    cursor,
                },
                7,
            )
        })
        .unwrap()
    };
    let p1 = page(None);
    assert_eq!(p1.rows.len(), 500);
    assert_eq!(
        p1.rows[0].created_at, 1200,
        "newest first when no position is set"
    );
    let p2 = page(p1.next_cursor.clone());
    assert_eq!(p2.rows.len(), 500);
    let p3 = page(p2.next_cursor.clone());
    assert_eq!(p3.rows.len(), 201);
    assert!(p3.next_cursor.is_none());
    let mut all: Vec<String> = [p1.rows, p2.rows, p3.rows]
        .concat()
        .into_iter()
        .map(|r| r.chat_id)
        .collect();
    all.sort();
    all.dedup();
    assert_eq!(all.len(), 1201);
}

#[test]
fn views_split_active_archived_and_hide_side_chats() {
    let (_d, db) = temp_db();
    let a = chat_in(&db, "C:/w/app", 1);
    let b = chat_in(&db, "C:/w/app", 2);
    let side = chat_in(&db, "C:/w/app", 3);
    db.write(|tx| {
        tx.execute("UPDATE chats SET archived = 1 WHERE id = ?1", [&b])?;
        tx.execute(
            "UPDATE chats SET side_source_id = ?2 WHERE id = ?1",
            [&side, &a],
        )
    })
    .unwrap();
    let ids = |view| -> Vec<String> {
        db.snapshot(|tx| {
            store::sidebar_page(
                tx,
                &SidebarPageArgs {
                    view,
                    group_id: None,
                    cursor: None,
                },
                1,
            )
        })
        .unwrap()
        .rows
        .into_iter()
        .map(|r| r.chat_id)
        .collect()
    };
    assert_eq!(ids(SidebarView::Projects), std::slice::from_ref(&a));
    assert_eq!(ids(SidebarView::Activity), [a]);
    assert_eq!(ids(SidebarView::Archived), [b]);
}

// ---- SB6 status and organisation -----------------------------------------------------------

#[test]
fn row_status_priority_is_running_unread_lingering_interrupted_pinned() {
    use RowAttention::*;
    let f = row_attention;
    assert_eq!(f(ChatStatus::Running, true, true, true), Some(Running));
    assert_eq!(f(ChatStatus::Stopping, false, false, false), Some(Running));
    assert_eq!(f(ChatStatus::Interrupted, true, true, true), Some(Unread));
    assert_eq!(
        f(ChatStatus::Interrupted, false, true, true),
        Some(Lingering)
    );
    assert_eq!(
        f(ChatStatus::Interrupted, false, false, true),
        Some(Interrupted)
    );
    assert_eq!(f(ChatStatus::Idle, false, false, true), Some(Pinned));
    assert_eq!(f(ChatStatus::Idle, false, false, false), None);
}

fn organize(db: &Database, a: ChatOrganizeArgs) -> Result<Vec<String>, Refusal> {
    db.write(|tx| store::organize(tx, &a))
}

fn row(db: &Database, id: &str) -> SidebarRow {
    db.snapshot(|tx| store::sidebar_rows(tx, &[id.to_owned()], 1))
        .unwrap()
        .pop()
        .unwrap()
}

#[test]
fn dragging_a_chat_onto_a_group_writes_chat_groups_and_never_moves_where_it_runs() {
    let (_d, db) = temp_db();
    let c = chat_in(&db, "C:/w/app", 1);
    let home = project_of(&db, &c);
    let board = board_project(&db, "b-1", "Board one");
    organize(
        &db,
        ChatOrganizeArgs {
            chat_id: c.clone(),
            group_project_id: Some(board.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let r = row(&db, &c);
    assert_eq!(r.group_project_id, board);
    assert_eq!(r.folder_project_id, home, "where it runs is unchanged");
    assert_eq!(node(&db, &board).chat_count, 1);
    assert_eq!(node(&db, &home).chat_count, 0);
    // Dropping it back on its folder project removes the row.
    organize(
        &db,
        ChatOrganizeArgs {
            chat_id: c.clone(),
            group_project_id: Some(home.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let n: i64 = db
        .snapshot(|tx| tx.query_row("SELECT count(*) FROM chat_groups", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn reorder_stays_within_the_group_and_pin_state() {
    let (_d, db) = temp_db();
    let a = chat_in(&db, "C:/w/app", 1);
    let b = chat_in(&db, "C:/w/app", 2);
    let c = chat_in(&db, "C:/w/app", 3);
    let other = chat_in(&db, "C:/w/other", 4);
    // Order is newest first: c, b, a. Move a before c.
    organize(
        &db,
        ChatOrganizeArgs {
            chat_id: a.clone(),
            before_chat_id: Some(c.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let order: Vec<String> = db
        .snapshot(|tx| {
            store::sidebar_page(
                tx,
                &SidebarPageArgs {
                    view: SidebarView::Projects,
                    group_id: Some(project_of(&db, &a)),
                    cursor: None,
                },
                1,
            )
        })
        .unwrap()
        .rows
        .into_iter()
        .map(|r| r.chat_id)
        .collect();
    assert_eq!(order, [a.clone(), c.clone(), b.clone()]);
    // Not across groups, not across pin states.
    assert!(organize(
        &db,
        ChatOrganizeArgs {
            chat_id: a.clone(),
            before_chat_id: Some(other),
            ..Default::default()
        }
    )
    .is_err());
    organize(
        &db,
        ChatOrganizeArgs {
            chat_id: b.clone(),
            pinned: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(organize(
        &db,
        ChatOrganizeArgs {
            chat_id: a.clone(),
            before_chat_id: Some(b.clone()),
            ..Default::default()
        }
    )
    .is_err());
    assert_eq!(row(&db, &b).attention, Some(RowAttention::Pinned));
}

#[test]
fn chat_nesting_stops_at_depth_index_2_and_refuses_cycles() {
    let (_d, db) = temp_db();
    let a = chat_in(&db, "C:/w/app", 1);
    let b = chat_in(&db, "C:/w/app", 2);
    let c = chat_in(&db, "C:/w/app", 3);
    let d = chat_in(&db, "C:/w/app", 4);
    let x = chat_in(&db, "C:/w/elsewhere", 5);
    let nest = |child: &str, parent: Option<&str>| {
        organize(
            &db,
            ChatOrganizeArgs {
                chat_id: child.into(),
                parent_chat_id: Some(parent.map(Into::into)),
                ..Default::default()
            },
        )
    };
    nest(&b, Some(&a)).unwrap();
    nest(&c, Some(&b)).unwrap();
    assert_eq!(row(&db, &c).depth, 2);
    assert!(row(&db, &a).has_children);
    assert!(nest(&d, Some(&c)).is_err(), "depth index 3");
    assert!(nest(&a, Some(&c)).is_err(), "cycle");
    assert!(nest(&a, Some(&a)).is_err(), "self");
    assert!(nest(&x, Some(&a)).is_err(), "another group");
    nest(&c, None).unwrap();
    assert_eq!(row(&db, &c).depth, 0);
}

// ---- SB2/SB3/SB4 the read-only mirror --------------------------------------------------------

use crate::board::{apply_sync, read_state, record_result};
use helm_standup::BoardProject;

const BOARD: &str = "https://board.example";

fn bp(id: &str, title: &str, last: &str) -> BoardProject {
    BoardProject {
        id: id.into(),
        title: title.into(),
        headline: None,
        area: Some("desktop-app".into()),
        repo: None,
        priority: Some("p2".into()),
        last_activity: Some(last.into()),
        total: None,
        finished: None,
    }
}

/// `bp` with the board's work counts: `total` items under it, `finished` of them terminal.
fn counted(id: &str, last: &str, total: u64, finished: u64) -> BoardProject {
    BoardProject {
        total: Some(total),
        finished: Some(finished),
        ..bp(id, id, last)
    }
}

fn completed(db: &Database, board_id: &str) -> bool {
    by_board_id(db, board_id).board.unwrap().completed
}

fn sync(db: &Database, projects: &[BoardProject]) {
    db.write(|tx| apply_sync(tx, BOARD, projects, true, 5))
        .unwrap();
}

fn by_board_id(db: &Database, board_id: &str) -> ProjectNode {
    db.snapshot(|tx| store::list_projects(tx, true))
        .unwrap()
        .into_iter()
        .find(|p| {
            p.board
                .as_ref()
                .is_some_and(|b| b.board_project_id == board_id)
        })
        .unwrap()
}

#[test]
fn a_sync_mirrors_board_projects_at_the_top_level_and_updates_them() {
    let (_d, db) = temp_db();
    sync(
        &db,
        &[
            bp("a", "Alpha", "2026-10-17T10:00:00Z"),
            bp("b", "Beta", "2026-10-17T10:00:00Z"),
        ],
    );
    let a = by_board_id(&db, "a");
    assert_eq!(a.kind, ProjectKind::Board);
    assert_eq!(a.name, "Alpha");
    assert!(a.parent_id.is_none());
    assert_eq!(
        db.snapshot(|tx| read_state(tx, true, Some(BOARD)))
            .unwrap()
            .project_count,
        2
    );
    sync(
        &db,
        &[
            bp("a", "Alpha 2", "2026-10-17T11:00:00Z"),
            bp("b", "Beta", "2026-10-17T10:00:00Z"),
        ],
    );
    assert_eq!(by_board_id(&db, "a").name, "Alpha 2");
    assert_eq!(
        by_board_id(&db, "a").id,
        a.id,
        "the same local project, not a new one"
    );
}

#[test]
fn an_older_last_activity_is_ignored() {
    let (_d, db) = temp_db();
    sync(&db, &[bp("a", "New title", "2026-10-17T12:00:00Z")]);
    sync(&db, &[bp("a", "Old title", "2026-10-17T09:00:00Z")]);
    assert_eq!(by_board_id(&db, "a").name, "New title");
}

#[test]
fn completion_is_mirrored_and_refreshed_on_every_sync() {
    let (_d, db) = temp_db();
    let t = "2026-10-17T10:00:00Z";
    sync(
        &db,
        &[
            counted("done", t, 3, 3),
            counted("live", t, 3, 1),
            counted("childless", t, 0, 0),
            bp("uncounted", "Uncounted", t),
        ],
    );
    assert!(completed(&db, "done"));
    assert!(!completed(&db, "live"));
    assert!(
        !completed(&db, "childless"),
        "nothing under it is not finished"
    );
    assert!(
        !completed(&db, "uncounted"),
        "an older board sends no counts"
    );
    // The same activity time: the work finishes, or is reopened, and the flag follows.
    sync(&db, &[counted("done", t, 4, 3), counted("live", t, 3, 3)]);
    assert!(
        !completed(&db, "done"),
        "reopened work shows the project again"
    );
    assert!(completed(&db, "live"));
    // An out-of-order read keeps the newer title but still refreshes completion, so a flag is
    // never held back by the activity time.
    sync(&db, &[bp("live", "Live", "2026-10-17T12:00:00Z")]);
    assert!(!completed(&db, "live"));
    sync(&db, &[counted("live", "2026-10-17T09:00:00Z", 2, 2)]);
    assert!(completed(&db, "live"));
    assert_eq!(
        by_board_id(&db, "live").name,
        "Live",
        "the older title is still ignored"
    );
}

#[test]
fn a_profile_made_before_completion_gains_the_column_on_open() {
    use helm_core::db::MigrationRegistry;
    let registry = || {
        let mut reg = MigrationRegistry::new();
        helm_chats::register_migrations(&mut reg);
        crate::register_migrations(&mut reg);
        reg
    };
    let (dir, db) = temp_db();
    let path = dir.path().join("t.sqlite3");
    sync(&db, &[bp("a", "Alpha", "2026-10-17T10:00:00Z")]);
    // Put the profile back to how it was before the column existed.
    db.write(|tx| {
        tx.execute_batch(
            "ALTER TABLE board_projects DROP COLUMN completed;
             DELETE FROM counters WHERE key = 'migration:projects-board-completed';",
        )
    })
    .unwrap();
    drop(db);
    let db = Database::open(&path, &registry()).unwrap();
    let a = by_board_id(&db, "a");
    assert!(!a.board.unwrap().completed, "unset until the next sync");
    assert_eq!(a.name, "Alpha", "the mirrored row survives");
    sync(&db, &[counted("a", "2026-10-17T10:00:00Z", 1, 1)]);
    assert!(completed(&db, "a"));
    // Opening again is a no-op: the step ran once and the column is not added twice.
    drop(db);
    let db = Database::open(&path, &registry()).unwrap();
    assert!(completed(&db, "a"));
}

#[test]
fn a_vanished_project_goes_stale_and_its_chats_stay() {
    let (_d, db) = temp_db();
    sync(
        &db,
        &[
            bp("a", "Alpha", "2026-10-17T10:00:00Z"),
            bp("b", "Beta", "2026-10-17T10:00:00Z"),
        ],
    );
    let a = by_board_id(&db, "a");
    let c = chat_in(&db, "C:/w/app", 1);
    organize(
        &db,
        ChatOrganizeArgs {
            chat_id: c.clone(),
            group_project_id: Some(a.id.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    sync(&db, &[bp("b", "Beta", "2026-10-17T10:00:00Z")]);
    let a2 = by_board_id(&db, "a");
    assert!(a2.board.unwrap().stale);
    assert_eq!(
        row(&db, &c).group_project_id,
        a.id,
        "the chat stays under it"
    );
    assert_eq!(
        db.snapshot(|tx| read_state(tx, true, Some(BOARD)))
            .unwrap()
            .project_count,
        1
    );
    // It comes back when the board lists it again.
    sync(&db, &[bp("a", "Alpha", "2026-10-17T10:00:00Z")]);
    assert!(!by_board_id(&db, "a").board.unwrap().stale);
}

#[test]
fn local_edits_and_a_three_level_local_tree_survive_a_resync() {
    let (_d, db) = temp_db();
    let all = [
        bp("a", "Alpha", "2026-10-17T10:00:00Z"),
        bp("b", "Beta", "2026-10-17T10:00:00Z"),
        bp("c", "Gamma", "2026-10-17T10:00:00Z"),
    ];
    sync(&db, &all);
    let (a, b, c) = (
        by_board_id(&db, "a").id,
        by_board_id(&db, "b").id,
        by_board_id(&db, "c").id,
    );
    set_parent(&db, &b, Some(&a)).unwrap();
    set_parent(&db, &c, Some(&b)).unwrap();
    let mut e = upd(&a, &db);
    e.name = Some("My alpha".into());
    e.icon = Some(Some("🚀".into()));
    e.colour = Some(Some("teal".into()));
    e.pinned = Some(true);
    e.collapsed = Some(true);
    update(&db, e).unwrap();
    // The board renames it meanwhile.
    sync(
        &db,
        &[
            bp("a", "Alpha renamed", "2026-10-17T11:00:00Z"),
            all[1].clone(),
            all[2].clone(),
        ],
    );
    let n = node(&db, &a);
    assert_eq!(n.name, "My alpha", "the local rename wins");
    let board = n.board.clone().unwrap();
    assert_eq!(board.title, "Alpha renamed", "the board title is mirrored");
    assert!(board.edited_locally);
    assert_eq!(
        (
            n.icon.as_deref(),
            n.colour.as_deref(),
            n.pinned,
            n.collapsed
        ),
        (Some("🚀"), Some("teal"), true, true)
    );
    assert_eq!(node(&db, &b).parent_id.as_deref(), Some(a.as_str()));
    assert_eq!(node(&db, &c).parent_id.as_deref(), Some(b.as_str()));
    // Moving one to the top level is recorded as a local edit.
    set_parent(&db, &c, None).unwrap();
    let top: i64 = db
        .snapshot(|tx| {
            tx.query_row(
                "SELECT count(*) FROM project_top_level WHERE project_id = ?1",
                [&c],
                |r| r.get(0),
            )
        })
        .unwrap();
    assert_eq!(top, 1);
}

#[test]
fn a_failed_sync_keeps_the_last_good_time_and_shows_the_error() {
    let (_d, db) = temp_db();
    db.write(|tx| record_result(tx, 100, None)).unwrap();
    db.write(|tx| {
        record_result(
            tx,
            200,
            Some("Couldn't reach Agent Standup; showing the last sync."),
        )
    })
    .unwrap();
    let s = db.snapshot(|tx| read_state(tx, true, Some(BOARD))).unwrap();
    assert_eq!(s.last_sync_at, Some(100));
    assert_eq!(
        s.error.as_deref(),
        Some("Couldn't reach Agent Standup; showing the last sync.")
    );
    db.write(|tx| record_result(tx, 300, None)).unwrap();
    let s = db.snapshot(|tx| read_state(tx, true, Some(BOARD))).unwrap();
    assert_eq!((s.last_sync_at, s.error), (Some(300), None));
}

#[test]
fn a_sync_cut_short_marks_nothing_stale_and_counts_only_this_address() {
    let (_d, db) = temp_db();
    sync(
        &db,
        &[
            bp("a", "Alpha", "2026-10-17T10:00:00Z"),
            bp("b", "Beta", "2026-10-17T10:00:00Z"),
        ],
    );
    db.write(|tx| {
        apply_sync(
            tx,
            BOARD,
            &[bp("a", "Alpha", "2026-10-17T10:00:00Z")],
            false,
            6,
        )
    })
    .unwrap();
    assert!(
        !by_board_id(&db, "b").board.unwrap().stale,
        "a partial read is not proof b is gone"
    );
    db.write(|tx| {
        apply_sync(
            tx,
            "https://other.example",
            &[bp("z", "Other", "2026-10-17T10:00:00Z")],
            true,
            7,
        )
    })
    .unwrap();
    assert_eq!(
        db.snapshot(|tx| read_state(tx, true, Some(BOARD)))
            .unwrap()
            .project_count,
        2
    );
}

#[test]
fn rows_carry_unread_mute_and_stall_from_the_chat_record() {
    let (_d, db) = temp_db();
    let c = chat_in(&db, "C:/w/app", 1);
    assert!(!row(&db, &c).unread);
    db.write(|tx| {
        tx.execute(
            "UPDATE chats SET status = 'interrupted' WHERE id = ?1",
            [&c],
        )
    })
    .unwrap();
    assert_eq!(row(&db, &c).attention, Some(RowAttention::Interrupted));
    // Rows the chats crate writes in its own tables (test-only direct inserts).
    db.write(|tx| {
        tx.execute(
            "INSERT INTO unread_marks(chat_id, token, marked_at) VALUES (?1, 't', 1)",
            [&c],
        )?;
        tx.execute(
            "INSERT INTO chat_mutes(chat_id, muted_at) VALUES (?1, 1)",
            [&c],
        )
    })
    .unwrap();
    assert!(!row(&db, &c).has_draft);
    db.write(|tx| {
        tx.execute(
            "UPDATE chats SET draft = 'half a thought' WHERE id = ?1",
            [&c],
        )
    })
    .unwrap();
    let r = row(&db, &c);
    assert!(r.unread && r.muted && r.has_draft);
    assert_eq!(
        r.attention,
        Some(RowAttention::Unread),
        "unread outranks interrupted"
    );
}

fn message(
    db: &Database,
    chat: &str,
    key: &str,
    role: &str,
    text: &str,
    tool: Option<&str>,
    status: &str,
) {
    // Equal timestamps: the newest row is the one inserted last (ordered by timestamp, then id).
    db.write(|tx| {
        tx.execute(
            "INSERT INTO messages (chat_id, source_key, role, text, tool_name, status, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)",
            params![chat, key, role, text, tool, status],
        )
    })
    .unwrap();
}

fn finish_message(db: &Database, chat: &str, key: &str) {
    db.write(|tx| {
        tx.execute(
            "UPDATE messages SET status = 'complete' WHERE chat_id = ?1 AND source_key = ?2",
            [chat, key],
        )
    })
    .unwrap();
}

#[test]
fn the_hover_preview_carries_the_last_reply_and_while_busy_the_current_step() {
    let (_d, db) = temp_db();
    let c = chat_in(&db, "C:/w/app", 1);
    // No reply yet, idle: an empty preview, and only the by-id read fills it.
    assert_eq!(
        row(&db, &c).preview,
        Some(RowPreview {
            excerpt: None,
            step: None
        })
    );
    let page = db
        .snapshot(|tx| {
            store::sidebar_page(
                tx,
                &SidebarPageArgs {
                    view: SidebarView::Projects,
                    cursor: None,
                    group_id: None,
                },
                1,
            )
        })
        .unwrap();
    assert!(
        page.rows.iter().all(|r| r.preview.is_none()),
        "pages leave the preview out"
    );

    message(&db, &c, "u1", "user", "Fix the login", None, "complete");
    message(
        &db,
        &c,
        "a1",
        "assistant",
        "First   answer

with  lines",
        None,
        "complete",
    );
    message(&db, &c, "a2", "assistant", "", None, "complete"); // empty text: skipped
    assert_eq!(
        row(&db, &c).preview.unwrap().excerpt.as_deref(),
        Some("First answer with lines")
    );

    // Busy: the step follows the newest active message. A running tool is the host's streaming
    // toolResult row (chats actor, tool_execution_start); once it completes the model is thinking.
    status(&db, &c, "running");
    message(&db, &c, "t:1", "toolResult", "", Some("Bash"), "streaming");
    assert_eq!(
        row(&db, &c).preview.unwrap().step.as_deref(),
        Some("Running Bash")
    );
    finish_message(&db, &c, "t:1");
    assert_eq!(
        row(&db, &c).preview.unwrap().step.as_deref(),
        Some("Thinking…")
    );
    message(&db, &c, "a4", "assistant", "Done now", None, "streaming");
    let p = row(&db, &c).preview.unwrap();
    assert_eq!(p.step.as_deref(), Some("Writing a reply"));
    assert_eq!(p.excerpt.as_deref(), Some("Done now"));
    status(&db, &c, "idle");
    assert_eq!(row(&db, &c).preview.unwrap().step, None);
}

#[test]
fn a_long_reply_is_cut_at_the_excerpt_limit_with_an_ellipsis() {
    let long = "word ".repeat(200);
    let e = store::excerpt_of(&long);
    assert!(e.ends_with('…'));
    assert_eq!(e.chars().count(), store::EXCERPT_CHARS); // 279 kept (trailing space trimmed) + the ellipsis
    assert_eq!(
        store::excerpt_of(
            "  short 
 reply "
        ),
        "short reply"
    );
}
