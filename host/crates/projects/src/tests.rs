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
