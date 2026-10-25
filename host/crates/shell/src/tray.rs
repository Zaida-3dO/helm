//! The tray menu as data: a pure function of the attention summary,
//! the capture devices and the settings. The app turns it into a native menu; the probe records it.

use crate::types::{NotificationMode, ShellSettings};
use helm_chats::types::{AttentionRow, AttentionSummary};
use helm_platform::CaptureDevice;
use serde::Serialize;

/// Items per group.
pub const GROUP_MAX: usize = 8;
/// Title characters before the ellipsis.
pub const TITLE_MAX: usize = 60;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayMenu {
    pub groups: Vec<TrayGroup>,
    pub mics: Vec<MicEntry>,
    pub keep_awake: bool,
    pub notifications: NotificationMode,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayGroup {
    pub label: String,
    pub items: Vec<TrayChat>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayChat {
    pub chat_id: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MicEntry {
    /// None is "System default".
    pub id: Option<String>,
    pub label: String,
    pub checked: bool,
}

/// No stored choice: the system default capture device.
pub const SYSTEM_DEFAULT: &str = "Automatic";

/// "title · project", the title cut at 60 characters with an ellipsis, "Untitled" when blank.
pub fn chat_label(title: &str, project: &str) -> String {
    let t = title.trim();
    let mut label: String = if t.is_empty() {
        "Untitled".to_owned()
    } else if t.chars().count() > TITLE_MAX {
        let mut s: String = t.chars().take(TITLE_MAX).collect();
        s.push('…');
        s
    } else {
        t.to_owned()
    };
    if !project.trim().is_empty() {
        label.push_str(" · ");
        label.push_str(project.trim());
    }
    label
}

fn group(label: &str, rows: &[AttentionRow]) -> Option<TrayGroup> {
    if rows.is_empty() {
        return None;
    }
    Some(TrayGroup {
        label: label.to_owned(),
        items: rows
            .iter()
            .take(GROUP_MAX)
            .map(|r| TrayChat {
                chat_id: r.chat_id.clone(),
                label: chat_label(&r.title, &r.project),
            })
            .collect(),
    })
}

/// The microphone entries: "Automatic" first, then each active device. A stored choice whose
/// device is gone is listed as "<name>: not connected, using <device> for now" and unticked, and
/// Automatic is ticked, because that is what capture would use; the stored choice is kept.
pub fn mic_entries(devices: &[CaptureDevice], settings: &ShellSettings) -> Vec<MicEntry> {
    let chosen = settings.dictation_mic.as_ref();
    let present = chosen.is_some_and(|c| devices.iter().any(|d| d.id == c.uid));
    let mut out = vec![MicEntry {
        id: None,
        label: SYSTEM_DEFAULT.to_owned(),
        checked: !present,
    }];
    for d in devices {
        out.push(MicEntry {
            id: Some(d.id.clone()),
            label: d.name.clone(),
            checked: present && chosen.is_some_and(|c| c.uid == d.id),
        });
    }
    if let (Some(c), false) = (chosen, present) {
        out.push(MicEntry {
            id: Some(c.uid.clone()),
            label: format!(
                "{}: not connected, using {} for now",
                c.name,
                devices
                    .iter()
                    .find(|d| d.is_default)
                    .or(devices.first())
                    .map(|d| d.name.as_str())
                    .unwrap_or("no microphone")
            ),
            checked: false,
        });
    }
    out
}

pub fn menu_model(
    summary: Option<&AttentionSummary>,
    devices: &[CaptureDevice],
    settings: &ShellSettings,
) -> TrayMenu {
    let groups = summary
        .map(|s| {
            [
                group("Needs you", &s.needs_you),
                group("Running", &s.running),
                group("Unread", &s.unread),
            ]
            .into_iter()
            .flatten()
            .collect()
        })
        .unwrap_or_default();
    TrayMenu {
        groups,
        mics: mic_entries(devices, settings),
        keep_awake: settings.keep_awake,
        notifications: settings.notifications,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MicChoice;
    use helm_chats::types::AttentionKind;

    fn row(id: &str, title: &str, kind: AttentionKind) -> AttentionRow {
        AttentionRow {
            chat_id: id.into(),
            title: title.into(),
            project: "proj".into(),
            kind,
            reason: String::new(),
        }
    }

    fn dev(id: &str, name: &str, def: bool) -> CaptureDevice {
        CaptureDevice {
            id: id.into(),
            name: name.into(),
            is_default: def,
        }
    }

    #[test]
    fn labels_cut_titles_and_name_blank_ones() {
        assert_eq!(chat_label("  ", "p"), "Untitled · p");
        assert_eq!(chat_label("Fix it", ""), "Fix it");
        let long = "x".repeat(61);
        let l = chat_label(&long, "p");
        assert_eq!(l, format!("{}… · p", "x".repeat(60)));
        assert_eq!(chat_label(&"y".repeat(60), ""), "y".repeat(60));
    }

    #[test]
    fn groups_appear_in_order_only_when_non_empty_and_cap_at_eight() {
        let s = AttentionSummary {
            badge: 1,
            busy: 1,
            needs_you: vec![row("a", "A", AttentionKind::Approval)],
            running: (0..10)
                .map(|i| row(&format!("r{i}"), "R", AttentionKind::Running))
                .collect(),
            unread: vec![],
            stalled: vec![],
            flagged: vec![],
            revision: 1,
        };
        let m = menu_model(Some(&s), &[], &ShellSettings::default());
        let labels: Vec<_> = m.groups.iter().map(|g| g.label.as_str()).collect();
        assert_eq!(labels, ["Needs you", "Running"]);
        assert_eq!(m.groups[1].items.len(), 8);
        assert_eq!(m.groups[0].items[0].label, "A · proj");
        assert!(menu_model(None, &[], &ShellSettings::default())
            .groups
            .is_empty());
    }

    #[test]
    fn system_default_is_first_and_ticked_without_a_choice() {
        let m = mic_entries(
            &[dev("m1", "USB mic", false), dev("m2", "Array", true)],
            &ShellSettings::default(),
        );
        assert_eq!(m[0].label, SYSTEM_DEFAULT);
        assert!(m[0].checked);
        assert_eq!(m.len(), 3);
        assert!(!m[1].checked && !m[2].checked);
    }

    #[test]
    fn a_present_choice_is_ticked() {
        let s = ShellSettings {
            dictation_mic: Some(MicChoice {
                uid: "m1".into(),
                name: "USB mic".into(),
            }),
            ..Default::default()
        };
        let m = mic_entries(&[dev("m1", "USB mic", false)], &s);
        assert!(!m[0].checked);
        assert!(m[1].checked);
    }

    #[test]
    fn a_missing_choice_shows_unavailable_and_the_default_is_used() {
        let s = ShellSettings {
            dictation_mic: Some(MicChoice {
                uid: "gone".into(),
                name: "Headset".into(),
            }),
            ..Default::default()
        };
        let m = mic_entries(&[dev("m1", "USB mic", true)], &s);
        assert!(m[0].checked, "the default is what capture uses");
        let last = m.last().unwrap();
        assert_eq!(last.label, "Headset: not connected, using USB mic for now");
        assert_eq!(last.id.as_deref(), Some("gone"));
        assert!(!last.checked);
    }
}
