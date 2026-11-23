use std::collections::BTreeMap;

use super::sab_share::SabServer;
use crate::repositories::usenet_servers::UsenetServerRow;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountLoad {
    pub host: String,
    pub username: String,
    pub connections: i64,
    pub sab_connections: i64,
    pub cap: Option<i32>,
}

impl AccountLoad {
    pub fn over_cap(&self) -> bool {
        self.cap
            .is_some_and(|cap| self.connections + self.sab_connections > i64::from(cap))
    }

    pub fn describe(&self) -> String {
        let login = format!("The account on {}", self.host);
        let held = if self.sab_connections > 0 {
            format!(
                "{} connections ({} here plus SABnzbd's {})",
                self.connections + self.sab_connections,
                self.connections,
                self.sab_connections
            )
        } else {
            format!("{} connections", self.connections)
        };
        match self.cap {
            Some(cap) => format!("{login} would hold {held}, over its cap of {cap}"),
            None => format!("{login} holds {held}"),
        }
    }
}

pub fn same_host(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

fn identity(host: &str, username: &str) -> (String, String) {
    (host.trim().to_lowercase(), username.trim().to_string())
}

fn account(row: &UsenetServerRow) -> (String, String) {
    identity(&row.host, &row.username)
}

fn entry(
    by_account: &mut BTreeMap<(String, String), AccountLoad>,
    (host, username): (String, String),
) -> &mut AccountLoad {
    by_account
        .entry((host.clone(), username.clone()))
        .or_insert(AccountLoad {
            host,
            username,
            connections: 0,
            sab_connections: 0,
            cap: None,
        })
}

/// Disabled rows hold no connections, but their cap still describes the account. SAB's
/// servers only count where they share an account with ours: elsewhere no cap is known.
pub fn accounts(rows: &[UsenetServerRow], sab: &[SabServer]) -> Vec<AccountLoad> {
    let mut by_account: BTreeMap<(String, String), AccountLoad> = BTreeMap::new();
    for row in rows {
        let load = entry(&mut by_account, account(row));
        if row.enabled {
            load.connections += i64::from(row.connections);
        }
        load.cap = match (load.cap, row.account_connection_cap) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }
    for server in sab {
        if let Some(load) = by_account.get_mut(&identity(&server.host, &server.username)) {
            load.sab_connections += server.connections;
        }
    }
    by_account.into_values().collect()
}

/// The account `candidate` belongs to once it replaces its stored row, if that is over cap.
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
    let key = account(candidate);
    accounts(&rows, sab)
        .into_iter()
        .find(|a| (a.host.clone(), a.username.clone()) == key)
        .filter(AccountLoad::over_cap)
}
