use serde::{Deserialize, Serialize};

use crate::model::Address;

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    #[serde(default)]
    pub id: i64,
    pub account_id: i64,
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "yes")]
    pub match_all: bool,
    pub conditions: Vec<Condition>,
    pub actions: Vec<Action>,
    /// Later rules skip the messages this one matched.
    #[serde(default)]
    pub stop: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    From,
    To,
    Subject,
    ListId,
    HasAttachment,
    Bulk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Contains,
    NotContains,
    Is,
    EndsWith,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Condition {
    pub field: Field,
    pub op: Op,
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    Move { mailbox_id: i64 },
    Archive,
    Trash,
    MarkRead,
    Pin,
}

#[derive(Debug, Clone, Default)]
pub struct Candidate {
    pub id: i64,
    pub from: Option<Address>,
    pub recipients: Vec<Address>,
    pub subject: String,
    pub list_id: Option<String>,
    pub has_attachments: bool,
    /// Not from a person (notification, newsletter), same test as the smart inbox.
    pub bulk: bool,
    pub seen: bool,
    pub date: i64,
}

fn text(op: Op, values: &[&str], wanted: &str) -> bool {
    let wanted = wanted.trim().to_lowercase();
    // An empty value matches nothing, so a rule never catches everything by accident.
    if wanted.is_empty() {
        return false;
    }
    let values: Vec<String> = values.iter().map(|v| v.to_lowercase()).collect();
    match op {
        Op::Contains => values.iter().any(|v| v.contains(&wanted)),
        Op::NotContains => values.iter().all(|v| !v.contains(&wanted)),
        Op::Is => values.iter().any(|v| *v == wanted),
        Op::EndsWith => values.iter().any(|v| v.ends_with(&wanted)),
    }
}

fn address_values(a: &Address) -> [&str; 2] {
    [a.email.as_str(), a.name.as_deref().unwrap_or("")]
}

impl Condition {
    pub fn matches(&self, m: &Candidate) -> bool {
        match self.field {
            Field::From => {
                let values = m.from.as_ref().map(address_values).unwrap_or(["", ""]);
                text(self.op, &values, &self.value)
            }
            Field::To => {
                let values: Vec<&str> = m.recipients.iter().flat_map(address_values).collect();
                text(self.op, &values, &self.value)
            }
            Field::Subject => text(self.op, &[m.subject.as_str()], &self.value),
            Field::ListId => text(self.op, &[m.list_id.as_deref().unwrap_or("")], &self.value),
            Field::HasAttachment => m.has_attachments == (self.op != Op::NotContains),
            Field::Bulk => m.bulk == (self.op != Op::NotContains),
        }
    }
}

impl Rule {
    pub fn matches(&self, m: &Candidate) -> bool {
        if !self.enabled || self.conditions.is_empty() || self.actions.is_empty() {
            return false;
        }
        if self.match_all {
            self.conditions.iter().all(|c| c.matches(m))
        } else {
            self.conditions.iter().any(|c| c.matches(m))
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Preview {
    pub count: usize,
    pub unread: usize,
    pub with_attachments: usize,
    pub bulk: usize,
    pub oldest: Option<i64>,
    pub newest: Option<i64>,
    pub top_senders: Vec<crate::model::CountedAddress>,
    pub messages: Vec<crate::model::MessageSummary>,
}

/// At most one destination: the last matching rule that gives one wins.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decision {
    pub mark_read: bool,
    pub pin: bool,
    pub destination: Option<Action>,
}

pub fn decide(rules: &[Rule], m: &Candidate) -> Option<Decision> {
    let mut decision = Decision::default();
    let mut any = false;
    for rule in rules.iter().filter(|r| r.matches(m)) {
        any = true;
        for action in &rule.actions {
            match action {
                Action::MarkRead => decision.mark_read = true,
                Action::Pin => decision.pin = true,
                other => decision.destination = Some(other.clone()),
            }
        }
        if rule.stop {
            break;
        }
    }
    any.then_some(decision)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(conditions: Vec<Condition>, actions: Vec<Action>) -> Rule {
        Rule { id: 1, account_id: 1, name: "r".into(), enabled: true, match_all: true, conditions, actions, stop: false }
    }
    fn cond(field: Field, op: Op, value: &str) -> Condition {
        Condition { field, op, value: value.into() }
    }

    #[test]
    fn rules_decide() {
        let impots = Candidate {
            id: 1,
            from: Some(Address { name: Some("DGFiP".into()), email: "ne-pas-repondre@dgfip.finances.gouv.fr".into() }),
            subject: "Votre avis d'impôt".into(),
            bulk: true,
            ..Default::default()
        };
        let folder = rule(vec![cond(Field::From, Op::EndsWith, "finances.gouv.fr")], vec![Action::Move { mailbox_id: 9 }, Action::MarkRead]);
        let d = decide(&[folder.clone()], &impots).unwrap();
        assert_eq!(d, Decision { mark_read: true, pin: false, destination: Some(Action::Move { mailbox_id: 9 }) });

        let mut both = rule(vec![cond(Field::Subject, Op::Contains, "facture"), cond(Field::Bulk, Op::Is, "")], vec![Action::Archive]);
        assert!(!both.matches(&impots));
        both.match_all = false;
        assert!(both.matches(&impots), "bulk alone is enough");

        assert!(!rule(vec![cond(Field::Subject, Op::Contains, " ")], vec![Action::Trash]).matches(&impots));
        assert!(decide(&[Rule { enabled: false, ..folder.clone() }], &impots).is_none());

        let pin = rule(vec![cond(Field::From, Op::Contains, "dgfip")], vec![Action::Pin]);
        let stop = Rule { stop: true, ..folder };
        assert!(!decide(&[stop, pin], &impots).unwrap().pin);
    }
}
