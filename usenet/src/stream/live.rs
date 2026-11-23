use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use usenet_proto::StreamKind;

type Key = (String, usize, StreamKind);

struct Group {
    id: u64,
    name: String,
    len: u64,
    requests: usize,
    sent: u64,
    at: u64,
}

pub struct LiveEntry {
    pub id: u64,
    pub name: String,
    pub kind: StreamKind,
    pub position_percent: Option<u8>,
    pub sent: u64,
}

// A player's seeks and read-ahead arrive as many overlapping Range requests; they are one
// stream to whoever is watching the list.
#[derive(Default)]
pub struct LiveStreams {
    groups: Mutex<(u64, HashMap<Key, Group>)>,
}

impl LiveStreams {
    fn lock(&self) -> MutexGuard<'_, (u64, HashMap<Key, Group>)> {
        self.groups.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn enter(
        self: &Arc<Self>,
        release: String,
        output: usize,
        kind: StreamKind,
        name: &str,
        len: u64,
        at: u64,
    ) -> LiveStream {
        let key = (release, output, kind);
        let mut guard = self.lock();
        let (next, groups) = &mut *guard;
        let group = groups.entry(key.clone()).or_insert_with(|| {
            *next += 1;
            Group {
                id: *next,
                name: name.to_owned(),
                len,
                requests: 0,
                sent: 0,
                at,
            }
        });
        group.requests += 1;
        group.at = at;
        drop(guard);
        LiveStream {
            streams: Arc::clone(self),
            key,
        }
    }

    pub fn snapshot(&self) -> Vec<LiveEntry> {
        let guard = self.lock();
        let mut entries: Vec<LiveEntry> = guard
            .1
            .iter()
            .map(|((_, _, kind), group)| LiveEntry {
                id: group.id,
                name: group.name.clone(),
                kind: *kind,
                position_percent: percent(group.at, group.len),
                sent: group.sent,
            })
            .collect();
        drop(guard);
        entries.sort_by_key(|e| e.id);
        entries
    }
}

fn percent(at: u64, len: u64) -> Option<u8> {
    let share = u128::from(at.min(len)) * 100;
    u8::try_from(share.checked_div(u128::from(len))?).ok()
}

pub struct LiveStream {
    streams: Arc<LiveStreams>,
    key: Key,
}

impl LiveStream {
    pub fn advance(&self, at: u64, read: u64) {
        if let Some(group) = self.streams.lock().1.get_mut(&self.key) {
            group.at = at;
            group.sent += read;
        }
    }
}

// Held by the response body, so a client that disconnects ends the entry when hyper drops it.
impl Drop for LiveStream {
    fn drop(&mut self) {
        let mut guard = self.streams.lock();
        if let Some(group) = guard.1.get_mut(&self.key) {
            group.requests -= 1;
            if group.requests == 0 {
                guard.1.remove(&self.key);
            }
        }
    }
}
