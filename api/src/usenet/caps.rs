use std::collections::{BTreeMap, HashMap};

use uuid::Uuid;

use super::sab_share::SabServer;
use crate::repositories::usenet_servers::UsenetServerRow;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountLoad {
    pub host: String,
    pub connections: i64,
    pub servers: i64,
    pub sab_connections: i64,
    pub cap: Option<i32>,
}

impl AccountLoad {
    // SAB's share alone never makes an account over cap: the engine yields to it.
    pub fn over_cap(&self) -> bool {
        self.sab_leaves_no_room()
            || self
                .cap
                .is_some_and(|cap| self.connections > i64::from(cap))
    }

    // The engine shrinks to whatever SAB leaves, but never below one connection per server.
    pub fn sab_leaves_no_room(&self) -> bool {
        self.servers > 0
            && self.sab_connections > 0
            && self
                .cap
                .is_some_and(|cap| self.sab_connections + self.servers > i64::from(cap))
    }

    pub fn describe(&self) -> String {
        if self.sab_leaves_no_room() {
            return self.describe_sab();
        }
        let login = format!("The account on {}", self.host);
        match self.cap {
            Some(cap) => format!(
                "{login} would hold {} connections, over its cap of {cap}",
                self.connections
            ),
            None => format!("{login} holds {} connections", self.connections),
        }
    }

    pub fn describe_sab(&self) -> String {
        format!(
            "The account on {} would hold SABnzbd's {} connections plus at least {} for the usenet engine, over its cap of {}. Lower SABnzbd's connections for this server first.",
            self.host,
            self.sab_connections,
            self.servers,
            self.cap.unwrap_or_default()
        )
    }
}

pub fn same_host(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

fn host_key(host: &str) -> String {
    host.trim().to_lowercase()
}

// One login reached through several hosts of a backbone is still one account with one cap.
// Without a login there is nothing to say two servers share an account.
fn account(row: &UsenetServerRow) -> (String, String) {
    let username = row.username.trim();
    let provider = row
        .backbone
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty() && !username.is_empty())
        .map_or_else(
            || host_key(&row.host),
            |b| format!("backbone:{}", b.to_lowercase()),
        );
    (provider, username.to_string())
}

// SAB knows no backbones, so its server joins whichever account one of our rows reaches
// through the same host with the same login.
fn sab_account(rows: &[UsenetServerRow], server: &SabServer) -> Option<(String, String)> {
    rows.iter()
        .find(|r| same_host(&r.host, &server.host) && r.username.trim() == server.username.trim())
        .map(account)
}

// Disabled rows hold no connections, but their cap still describes the account. SAB's
// servers only count where they share an account with ours: elsewhere no cap is known.
fn by_account(
    rows: &[UsenetServerRow],
    sab: &[SabServer],
) -> BTreeMap<(String, String), AccountLoad> {
    let mut loads: BTreeMap<(String, String), AccountLoad> = BTreeMap::new();
    for row in rows {
        let load = loads.entry(account(row)).or_insert_with(|| AccountLoad {
            host: row.host.trim().to_string(),
            connections: 0,
            servers: 0,
            sab_connections: 0,
            cap: None,
        });
        if row.enabled {
            load.connections += i64::from(row.connections);
            load.servers += 1;
        }
        load.cap = match (load.cap, row.account_connection_cap) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }
    for server in sab {
        if let Some(load) = sab_account(rows, server).and_then(|key| loads.get_mut(&key)) {
            load.sab_connections += server.connections;
        }
    }
    loads
}

pub fn accounts(rows: &[UsenetServerRow], sab: &[SabServer]) -> Vec<AccountLoad> {
    by_account(rows, sab).into_values().collect()
}

pub fn violation(
    stored: &[UsenetServerRow],
    candidate: &UsenetServerRow,
    sab: &[SabServer],
) -> Option<AccountLoad> {
    let mut rows: Vec<UsenetServerRow> = stored
        .iter()
        .filter(|r| r.id != candidate.id)
        .cloned()
        .collect();
    rows.push(candidate.clone());
    by_account(&rows, sab)
        .remove(&account(candidate))
        .filter(AccountLoad::over_cap)
}

pub fn sab_violation(rows: &[UsenetServerRow], sab: &[SabServer]) -> Option<AccountLoad> {
    accounts(rows, sab)
        .into_iter()
        .find(AccountLoad::sab_leaves_no_room)
}

/// Connections each enabled server may open while SAB holds its share of the same account:
/// the account's room goes to its servers in priority order, keeping one for each server
/// still to come.
pub fn engine_connections(rows: &[UsenetServerRow], sab: &[SabServer]) -> HashMap<Uuid, i32> {
    let mut room: HashMap<(String, String), (i64, i64)> = by_account(rows, sab)
        .into_iter()
        .filter(|(_, load)| load.sab_connections > 0)
        .filter_map(|(key, load)| {
            load.cap
                .map(|cap| (key, (i64::from(cap) - load.sab_connections, load.servers)))
        })
        .collect();
    let mut enabled: Vec<&UsenetServerRow> = rows.iter().filter(|r| r.enabled).collect();
    enabled.sort_by_key(|r| r.priority);
    enabled
        .into_iter()
        .map(|row| {
            let connections = match room.get_mut(&account(row)) {
                Some((left, servers)) => {
                    *servers -= 1;
                    let given = i64::from(row.connections).min(*left - *servers).max(1);
                    *left -= given;
                    i32::try_from(given).unwrap_or(row.connections)
                }
                None => row.connections,
            };
            (row.id, connections)
        })
        .collect()
}
