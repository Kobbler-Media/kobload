use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplainedSlot {
    Class,
    Session,
    Event,
    Place,
    Round,
    Date,
    Season,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlotMatch {
    pub slot: ExplainedSlot,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub weak: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Contradiction {
    pub slot: ExplainedSlot,
    pub value: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MatchExplanation {
    pub matched: Vec<SlotMatch>,
    pub contradictions: Vec<Contradiction>,
    pub unexplained: Vec<String>,
}

impl SlotMatch {
    pub(super) fn new(slot: ExplainedSlot, value: impl ToString) -> Self {
        Self {
            slot,
            value: value.to_string(),
            alias: None,
            weak: false,
        }
    }

    pub(super) fn on(mut self, alias: impl Into<String>) -> Self {
        self.alias = Some(alias.into());
        self
    }
}

impl Contradiction {
    pub(super) fn new(slot: ExplainedSlot, value: impl ToString) -> Self {
        Self {
            slot,
            value: value.to_string(),
        }
    }
}

impl ExplainedSlot {
    fn name(self) -> &'static str {
        match self {
            Self::Class => "class",
            Self::Session => "session",
            Self::Event => "event",
            Self::Place => "place",
            Self::Round => "round",
            Self::Date => "date",
            Self::Season => "season",
        }
    }
}

impl MatchExplanation {
    /// The `matched_pattern` text of a sport match. Its wording predates the
    /// explanation and the golden replay compares it, so it keeps leaving out
    /// the season and naming an event by its first word.
    pub fn summary(&self) -> String {
        let hits: Vec<String> = self
            .matched
            .iter()
            .filter_map(|m| {
                let value = match m.slot {
                    ExplainedSlot::Season => return None,
                    ExplainedSlot::Event => m.value.split(' ').next().unwrap_or_default(),
                    _ => m.value.as_str(),
                };
                let strength = match (m.slot, m.weak) {
                    (ExplainedSlot::Place, true) => "(weak)",
                    (ExplainedSlot::Place, false) => "(strong)",
                    _ => "",
                };
                Some(format!("{}={value}{strength}", m.slot.name()))
            })
            .collect();
        let against = if self.contradictions.is_empty() {
            String::new()
        } else {
            let notes: Vec<String> = self
                .contradictions
                .iter()
                .map(|c| format!("{} {}", c.slot.name(), c.value))
                .collect();
            format!(" against [{}]", notes.join(", "))
        };
        format!("sport [{}]{against}", hits.join(" "))
    }
}
