//! How filing works per provider: a generic IMAP server keeps each message in one folder, Gmail
//! gives it labels that its folders show. See docs/design/providers.md.

use crate::model::Role;
use crate::ops::Op;
use crate::store::MailboxState;
use crate::sync::Caps;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Provider {
    #[default]
    Imap,
    /// Gmail and Google Workspace, detected by the X-GM-EXT-1 capability.
    Gmail,
}

impl Provider {
    pub fn from_caps(caps: &Caps) -> Provider {
        if caps.gmail { Provider::Gmail } else { Provider::Imap }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Imap => "imap",
            Provider::Gmail => "gmail",
        }
    }

    pub fn parse(s: &str) -> Option<Provider> {
        match s {
            "imap" => Some(Provider::Imap),
            "gmail" => Some(Provider::Gmail),
            _ => None,
        }
    }
}

/// For folder-based servers; Gmail labels go through [`plan_labels`].
pub fn plan_move(items: &[Moving], dest: &MailboxState, uidvalidity: impl Fn(i64) -> Option<u32>) -> MovePlan {
    let mut plan = MovePlan::default();
    for item in items {
        plan.moved.push(item.id);
        match plan.commands.iter_mut().find(|c| c.source == item.mailbox_id) {
            Some(c) => {
                if let Op::MoveSet { uids, .. } = &mut c.op {
                    uids.push(item.uid);
                }
            }
            None => plan.commands.push(Command {
                source: item.mailbox_id,
                first: item.id,
                op: Op::MoveSet { mailbox: item.mailbox.clone(), uids: vec![item.uid], dest: dest.name.clone(), uidvalidity: uidvalidity(item.mailbox_id) },
            }),
        }
    }
    plan
}

#[derive(Debug, Clone)]
pub struct Moving {
    pub id: i64,
    pub mailbox_id: i64,
    pub mailbox: String,
    pub uid: u32,
}

#[derive(Debug, Default)]
pub struct MovePlan {
    pub moved: Vec<i64>,
    pub commands: Vec<Command>,
}

#[derive(Debug)]
pub struct Command {
    pub source: i64,
    /// Names the command in the queue.
    pub first: i64,
    pub op: Op,
}

impl Command {
    pub fn count(&self) -> usize {
        match &self.op {
            Op::MoveSet { uids, .. } | Op::StoreSet { uids, .. } | Op::DeleteSet { uids, .. } | Op::Labels { uids, .. } => uids.len(),
            _ => 1,
        }
    }
}

/// `handle`: the folder actually holding the message on Gmail (All Mail, the bin or spam).
#[derive(Debug, Clone)]
pub struct Labelled {
    pub id: i64,
    pub handle: i64,
    pub handle_name: String,
    pub handle_role: Option<Role>,
    pub uid: u32,
    pub labels: Vec<i64>,
}

#[derive(Debug, Clone)]
pub struct LabelFolder {
    pub id: i64,
    pub name: String,
    pub role: Option<Role>,
}

#[derive(Debug, Default)]
pub struct LabelPlan {
    pub labels: Vec<(i64, Vec<i64>)>,
    /// Messages whose handle folder changes (into or out of the bin or spam).
    pub rehome: Vec<(i64, i64)>,
    pub commands: Vec<Command>,
}

/// Mirrors Gmail's own "Move to":
/// - Into the bin or spam, the message leaves All Mail and every label.
/// - Out of them, it goes back to All Mail with `dest`'s label.
/// - Otherwise it gains `dest`'s label (none for All Mail: archiving) and loses the labels it was
///   passing through: facteur's new senders, refused and snoozed folders, `from` (the label it
///   is moved from), and the inbox when moved from there, or from no folder in particular (a
///   rule, the screener, a cleanup).
pub fn plan_labels(items: &[Labelled], dest: &MailboxState, from: Option<i64>, folders: &[LabelFolder], all_mail: &MailboxState, uidvalidity: impl Fn(i64) -> Option<u32>) -> LabelPlan {
    let mut plan = LabelPlan::default();
    let folder = |id: i64| folders.iter().find(|f| f.id == id);
    let name = |id: i64| folder(id).map(|f| gmail_label(&f.name, f.role));
    let from_inbox = from.is_none_or(|from| folder(from).is_some_and(|f| f.role == Some(Role::Inbox)));
    let passing: Vec<i64> = folders
        .iter()
        .filter(|f| {
            matches!(f.role, Some(Role::Screener | Role::Snoozed | Role::Refused)) || Some(f.id) == from || from_inbox && f.role == Some(Role::Inbox)
        })
        .map(|f| f.id)
        .filter(|id| *id != dest.id)
        .collect();
    let bin = |role: Option<Role>| matches!(role, Some(Role::Trash | Role::Junk));
    for item in items {
        if bin(dest.role) || bin(item.handle_role) {
            if item.handle == dest.id {
                continue;
            }
            let (home, labels) = if bin(dest.role) { (dest.id, Vec::new()) } else { (all_mail.id, dest.is_label.then_some(dest.id).into_iter().collect()) };
            plan.rehome.push((item.id, home));
            plan.labels.push((item.id, labels));
            let op = Op::MoveSet { mailbox: item.handle_name.clone(), uids: vec![item.uid], dest: dest.name.clone(), uidvalidity: uidvalidity(item.handle) };
            push(&mut plan.commands, item, op);
            continue;
        }
        let added: Vec<i64> = (dest.is_label && !item.labels.contains(&dest.id)).then_some(dest.id).into_iter().collect();
        let removed: Vec<i64> = item.labels.iter().copied().filter(|l| passing.contains(l)).collect();
        if added.is_empty() && removed.is_empty() {
            continue;
        }
        let mut labels: Vec<i64> = item.labels.iter().copied().filter(|l| !removed.contains(l)).chain(added.iter().copied()).collect();
        labels.sort_unstable();
        plan.labels.push((item.id, labels));
        let op = Op::Labels {
            mailbox: item.handle_name.clone(),
            uids: vec![item.uid],
            add: added.iter().filter_map(|id| name(*id)).collect(),
            remove: removed.iter().filter_map(|id| name(*id)).collect(),
            uidvalidity: uidvalidity(item.handle),
        };
        push(&mut plan.commands, item, op);
    }
    plan
}

/// Excludes system labels (inbox, Sent, Starred, Important...) and facteur's own folders
/// (screener, refused, snoozed), which only gestures set.
pub fn user_label(folder: &LabelFolder) -> bool {
    folder.role.is_none() && !folder.name.starts_with("[Gmail]/") && !folder.name.starts_with("[Google Mail]/")
}

/// Messages in the bin or spam have no labels to change; a label both added and removed is left
/// as it is.
pub fn plan_relabel(items: &[Labelled], add: &[i64], remove: &[i64], folders: &[LabelFolder], uidvalidity: impl Fn(i64) -> Option<u32>) -> LabelPlan {
    let mut plan = LabelPlan::default();
    let pickable = |id: &i64| folders.iter().any(|f| f.id == *id && user_label(f));
    let (add, remove): (Vec<i64>, Vec<i64>) = (
        add.iter().copied().filter(|id| pickable(id) && !remove.contains(id)).collect(),
        remove.iter().copied().filter(|id| pickable(id) && !add.contains(id)).collect(),
    );
    let name = |id: &i64| folders.iter().find(|f| f.id == *id).map(|f| gmail_label(&f.name, f.role));
    for item in items {
        if matches!(item.handle_role, Some(Role::Trash | Role::Junk)) {
            continue;
        }
        let added: Vec<i64> = add.iter().copied().filter(|l| !item.labels.contains(l)).collect();
        let removed: Vec<i64> = remove.iter().copied().filter(|l| item.labels.contains(l)).collect();
        if added.is_empty() && removed.is_empty() {
            continue;
        }
        let mut labels: Vec<i64> = item.labels.iter().copied().filter(|l| !removed.contains(l)).chain(added.iter().copied()).collect();
        labels.sort_unstable();
        plan.labels.push((item.id, labels));
        let op = Op::Labels {
            mailbox: item.handle_name.clone(),
            uids: vec![item.uid],
            add: added.iter().filter_map(name).collect(),
            remove: removed.iter().filter_map(name).collect(),
            uidvalidity: uidvalidity(item.handle),
        };
        push(&mut plan.commands, item, op);
    }
    plan
}

fn push(commands: &mut Vec<Command>, item: &Labelled, op: Op) {
    let same = |planned: &Op| match (planned, &op) {
        (Op::MoveSet { mailbox: a, dest: d, .. }, Op::MoveSet { mailbox: b, dest: e, .. }) => a == b && d == e,
        (Op::Labels { mailbox: a, add: x, remove: y, .. }, Op::Labels { mailbox: b, add: v, remove: w, .. }) => a == b && x == v && y == w,
        _ => false,
    };
    match commands.iter_mut().find(|c| same(&c.op)) {
        Some(Command { op: Op::MoveSet { uids, .. } | Op::Labels { uids, .. }, .. }) => uids.push(item.uid),
        _ => commands.push(Command { source: item.handle, first: item.id, op }),
    }
}

/// X-GM-LABELS names system labels by flag (`\Inbox`, `\Sent`...), the others by quoted name.
pub fn gmail_label(name: &str, role: Option<Role>) -> String {
    match role {
        Some(Role::Inbox) => return "\\Inbox".into(),
        Some(Role::Sent) => return "\\Sent".into(),
        Some(Role::Drafts) => return "\\Draft".into(),
        Some(Role::Flagged) => return "\\Starred".into(),
        Some(Role::Important) => return "\\Important".into(),
        _ => {}
    }
    let leaf = name.rsplit('/').next().unwrap_or(name);
    if (name.starts_with("[Gmail]/") || name.starts_with("[Google Mail]/")) && leaf.eq_ignore_ascii_case("Important") {
        return "\\Important".into();
    }
    format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A system label must be quoted in SEARCH: unquoted (`X-GM-LABELS \Inbox`), Gmail finds nothing.
pub fn gmail_label_search(name: &str, role: Option<Role>) -> String {
    let label = gmail_label(name, role);
    if label.starts_with('\\') {
        return format!("X-GM-LABELS \"{}\"", label.replace('\\', "\\\\"));
    }
    format!("X-GM-LABELS {label}")
}

/// Matches the label as the server's parsed answers give it.
pub fn unquote_label(label: &str) -> String {
    match label.strip_prefix('"').and_then(|l| l.strip_suffix('"')) {
        Some(inner) => inner.replace("\\\"", "\"").replace("\\\\", "\\"),
        None => label.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(id: i64, name: &str, role: Option<Role>) -> MailboxState {
        MailboxState {
            id,
            account_id: 1,
            name: name.into(),
            role,
            selectable: true,
            uidvalidity: Some(7),
            uidnext: None,
            highestmodseq: None,
            sync_from_uid: None,
            server_count: None,
            is_label: false,
        }
    }

    fn moving(id: i64, mailbox_id: i64, mailbox: &str, uid: u32) -> Moving {
        Moving { id, mailbox_id, mailbox: mailbox.into(), uid }
    }

    #[test]
    fn a_move_takes_one_command_per_source_folder() {
        let dest = state(9, "Mairie", None);
        let items = [moving(1, 2, "INBOX", 10), moving(2, 3, "Archive", 20), moving(3, 2, "INBOX", 11)];
        let plan = plan_move(&items, &dest, |_| Some(5));
        assert_eq!(plan.moved, vec![1, 2, 3]);
        let commands: Vec<_> = plan.commands.iter().map(|c| (c.source, c.first, c.count())).collect();
        assert_eq!(commands, vec![(2, 1, 2), (3, 2, 1)]);
        assert!(matches!(&plan.commands[0].op, Op::MoveSet { uids, dest, uidvalidity: Some(5), .. } if *uids == vec![10, 11] && dest == "Mairie"));
    }

    fn label(id: i64, name: &str, role: Option<Role>) -> LabelFolder {
        LabelFolder { id, name: name.into(), role }
    }

    fn labelled(id: i64, uid: u32, labels: &[i64]) -> Labelled {
        Labelled { id, handle: 1, handle_name: "[Gmail]/All Mail".into(), handle_role: Some(Role::All), uid, labels: labels.to_vec() }
    }

    #[test]
    fn on_gmail_labels_filing_changes_labels_as_gmail_does() {
        let all = state(1, "[Gmail]/All Mail", Some(Role::All));
        let folders = [label(2, "INBOX", Some(Role::Inbox)), label(3, "Mairie", None), label(4, "Banque", None), label(5, "[Gmail]/Important", None)];
        let as_label = |s: MailboxState| MailboxState { is_label: true, ..s };
        let mairie = as_label(state(3, "Mairie", None));
        let banque = as_label(state(4, "Banque", None));

        // From the inbox to a label: the inbox goes, Important stays; one command for the lot.
        let items = [labelled(10, 100, &[2, 5]), labelled(11, 101, &[2, 5])];
        let plan = plan_labels(&items, &mairie, Some(2), &folders, &all, |_| Some(7));
        assert_eq!(plan.labels, vec![(10, vec![3, 5]), (11, vec![3, 5])]);
        assert_eq!(plan.commands.len(), 1);
        assert!(matches!(&plan.commands[0].op, Op::Labels { mailbox, uids, add, remove, uidvalidity: Some(7) }
            if mailbox == "[Gmail]/All Mail" && *uids == vec![100, 101] && *add == vec!["\"Mairie\"".to_string()] && *remove == vec!["\\Inbox".to_string()]));

        // From one label to another: the first one goes, the inbox stays.
        let plan = plan_labels(&[labelled(10, 100, &[2, 3])], &banque, Some(3), &folders, &all, |_| None);
        assert_eq!(plan.labels, vec![(10, vec![2, 4])]);

        // Archiving: the inbox goes. Already archived: nothing to do.
        let plan = plan_labels(&[labelled(10, 100, &[2, 3]), labelled(11, 101, &[3])], &all, None, &folders, &all, |_| None);
        assert_eq!((plan.labels, plan.commands.len()), (vec![(10, vec![3])], 1));

        // Into the bin: out of All Mail and every label. Out of it, back with the label.
        let trash = state(6, "[Gmail]/Trash", Some(Role::Trash));
        let plan = plan_labels(&[labelled(10, 100, &[2, 3])], &trash, None, &folders, &all, |_| None);
        assert_eq!((plan.rehome, plan.labels), (vec![(10, 6)], vec![(10, vec![])]));
        assert!(matches!(&plan.commands[0].op, Op::MoveSet { mailbox, dest, .. } if mailbox == "[Gmail]/All Mail" && dest == "[Gmail]/Trash"));
        let binned = Labelled { handle: 6, handle_name: "[Gmail]/Trash".into(), handle_role: Some(Role::Trash), ..labelled(10, 7, &[]) };
        let plan = plan_labels(&[binned], &mairie, None, &folders, &all, |_| None);
        assert_eq!((plan.rehome, plan.labels), (vec![(10, 1)], vec![(10, vec![3])]));
        assert!(matches!(&plan.commands[0].op, Op::MoveSet { mailbox, dest, .. } if mailbox == "[Gmail]/Trash" && dest == "Mairie"));
    }

    #[test]
    fn labels_are_added_and_taken_without_filing() {
        let folders = [label(2, "INBOX", Some(Role::Inbox)), label(3, "Mairie", None), label(4, "Banque", None), label(5, "[Gmail]/Important", None)];
        assert_eq!(folders.iter().filter(|f| user_label(f)).map(|f| f.id).collect::<Vec<_>>(), [3, 4]);

        // Mairie added where missing, Banque taken where present; the inbox stays.
        let items = [labelled(10, 100, &[2, 4]), labelled(11, 101, &[2, 3, 4]), labelled(12, 102, &[2])];
        let plan = plan_relabel(&items, &[3], &[4], &folders, |_| Some(7));
        assert_eq!(plan.labels, vec![(10, vec![2, 3]), (11, vec![2, 3]), (12, vec![2, 3])]);
        assert_eq!(plan.commands.len(), 3, "one command per different change");
        assert!(matches!(&plan.commands[0].op, Op::Labels { add, remove, .. } if *add == vec!["\"Mairie\"".to_string()] && *remove == vec!["\"Banque\"".to_string()]));
        assert!(matches!(&plan.commands[2].op, Op::Labels { uids, add, remove, .. } if *uids == vec![102] && add.len() == 1 && remove.is_empty()));

        // The same change for several messages: one command.
        let plan = plan_relabel(&[labelled(10, 100, &[]), labelled(11, 101, &[])], &[3], &[], &folders, |_| None);
        assert!(matches!(&plan.commands[..], [Command { op: Op::Labels { uids, .. }, .. }] if *uids == vec![100, 101]));

        // System labels are not picked here, a binned message has none, and nothing to change is nothing.
        let binned = Labelled { handle: 6, handle_name: "[Gmail]/Trash".into(), handle_role: Some(Role::Trash), ..labelled(13, 7, &[]) };
        let plan = plan_relabel(&[labelled(10, 100, &[2]), binned], &[2, 5], &[], &folders, |_| None);
        assert!(plan.labels.is_empty() && plan.commands.is_empty());
        let plan = plan_relabel(&[labelled(10, 100, &[3])], &[3], &[3], &folders, |_| None);
        assert!(plan.labels.is_empty());
    }

    #[test]
    fn gmail_labels_are_named_as_gmail_names_them() {
        assert_eq!(gmail_label("INBOX", Some(Role::Inbox)), "\\Inbox");
        assert_eq!(gmail_label("[Gmail]/Messages envoy&AOk-s", Some(Role::Sent)), "\\Sent");
        assert_eq!(gmail_label("[Gmail]/Important", None), "\\Important");
        assert_eq!(gmail_label("[Gmail]/Wichtig", Some(Role::Important)), "\\Important", "whatever the language");
        assert_eq!(gmail_label("L&AOk-o &AOk-cole", None), "\"L&AOk-o &AOk-cole\"");
        assert_eq!(gmail_label("Boutiques/\"Amazon\"", None), "\"Boutiques/\\\"Amazon\\\"\"");
        assert_eq!(gmail_label_search("INBOX", Some(Role::Inbox)), "X-GM-LABELS \"\\\\Inbox\"");
        assert_eq!(gmail_label_search("Mairie", None), "X-GM-LABELS \"Mairie\"");
        for name in ["L&AOk-o &AOk-cole", "Boutiques/\"Amazon\"", "a\\b"] {
            assert_eq!(unquote_label(&gmail_label(name, None)), name);
        }
        assert_eq!(unquote_label("\\Inbox"), "\\Inbox");
    }
}
