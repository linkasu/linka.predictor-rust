use crate::{tokenize, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::Path,
};

const MAX_WORDS: usize = 10_000;
const MAX_EVENTS: usize = 20_000;
const MAX_BYTES: u64 = 10_000_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum EventOrigin {
    Confirmation,
    Selection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfirmedEvent {
    pub event_id: u64,
    pub utterance_id: u64,
    pub origin: EventOrigin,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersonalCounts {
    pub confirmed_count: u32,
    pub selection_count: u32,
    pub last_event: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub surface: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersonalModel {
    words: BTreeMap<String, PersonalCounts>,
    events: BTreeSet<u64>,
    actions: BTreeSet<(u64, EventOrigin, usize, usize, String)>,
}

impl PersonalModel {
    pub fn words(&self) -> impl Iterator<Item = (&str, &PersonalCounts)> {
        self.words.iter().map(|(w, c)| (w.as_str(), c))
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn observe(&mut self, event: ConfirmedEvent) -> Result<bool> {
        if event.text.len() > 16_384 {
            return Err("event exceeds 16 KiB".into());
        }
        if self.events.contains(&event.event_id) {
            return Ok(false);
        }
        // Retention is explicit: reject rather than forget IDs and double-count retries.
        if self.events.len() >= MAX_EVENTS {
            return Err("personal event retention exhausted; clear profile".into());
        }
        let mut next = self.clone();
        next.events.insert(event.event_id);
        for token in tokenize(&event.text) {
            if token.normalized.len() > 256 {
                continue;
            }
            if !next.actions.insert((
                event.utterance_id,
                event.origin,
                token.start_byte,
                token.end_byte,
                token.normalized.clone(),
            )) {
                continue;
            }
            let count = next.words.entry(token.normalized).or_default();
            match event.origin {
                EventOrigin::Confirmation => {
                    count.confirmed_count = count.confirmed_count.saturating_add(1);
                    let surface = &event.text[token.start_byte..token.end_byte];
                    if surface.len() <= 256 {
                        count.surface = surface.to_owned();
                    }
                }
                EventOrigin::Selection => {
                    count.selection_count = count.selection_count.saturating_add(1)
                }
            }
            count.last_event = event.event_id;
        }
        while next.words.len() > MAX_WORDS {
            let victim = next
                .words
                .iter()
                .min_by_key(|(w, c)| {
                    (
                        c.confirmed_count as u64 + c.selection_count as u64,
                        c.last_event,
                        (*w).clone(),
                    )
                })
                .unwrap()
                .0
                .clone();
            next.words.remove(&victim);
        }
        if serde_json::to_vec(&next)?.len() as u64 > MAX_BYTES {
            return Err("personal storage limit reached".into());
        }
        *self = next;
        Ok(true)
    }
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("personal storage limit reached".into());
        }
        crate::model::atomic_write(path.as_ref(), &bytes, true)
    }
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        if fs::metadata(path.as_ref())?.len() > MAX_BYTES {
            return Err("personal storage limit reached".into());
        }
        let mut bytes = Vec::new();
        fs::File::open(path)?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("personal storage limit reached".into());
        }
        let model: Self = serde_json::from_slice(&bytes)?;
        if model.words.len() > MAX_WORDS
            || model.events.len() > MAX_EVENTS
            || model.actions.len() > MAX_BYTES as usize / 10
        {
            return Err("invalid personal bounds".into());
        }
        if model.words.keys().any(|w| {
            let tokens = tokenize(w);
            w.is_empty()
                || w.len() > 256
                || crate::normalize(w) != *w
                || tokens.len() != 1
                || tokens[0].start_byte != 0
                || tokens[0].end_byte != w.len()
        }) {
            return Err("invalid personal word".into());
        }
        if model.words.iter().any(|(word, counts)| {
            let surface = &counts.surface;
            let tokens = tokenize(surface);
            !surface.is_empty()
                && (surface.len() > 256
                    || tokens.len() != 1
                    || tokens[0].start_byte != 0
                    || tokens[0].end_byte != surface.len()
                    || crate::normalize(surface) != *word)
        }) {
            return Err("invalid personal surface".into());
        }
        Ok(model)
    }
    pub(crate) fn surface(&self, word: &str) -> Option<&str> {
        self.words
            .get(word)
            .map(|c| c.surface.as_str())
            .filter(|surface| !surface.is_empty())
    }
    pub(crate) fn boost(&self, word: &str) -> f64 {
        self.words
            .get(word)
            .map(|c| {
                (0.15 * (1.0 + c.confirmed_count as f64).log10()
                    + 0.3 * (1.0 + c.selection_count as f64).log10())
                .min(1.5)
            })
            .unwrap_or(0.0)
    }
}
