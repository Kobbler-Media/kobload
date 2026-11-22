use std::collections::BTreeMap;

use crate::repositories::usenet_servers::UsenetServerRow;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountLoad {
    pub host: String,
    pub username: String,
    pub connections: i64,
    pub cap: Option<i32>,
}

impl AccountLoad {
    pub fn over_cap(&self) -> bool {
        self.cap
            .is_some_and(|cap| self.connections > i64::from(cap))
    }

    pub fn describe(&self) -> String {
        let login = if self.username.is_empty() {
            self.host.clone()
        } else {
            format!("{}@{}", self.username, self.host)
        };
        match self.cap {
            Some(cap) => format!(
                "{login} would hold {} connections, over its cap of {cap}",
                self.connections
            ),
            None => format!("{login} holds {} connections", self.connections),
        }
    }
}

fn account(row: &UsenetServerRow) -> (String, String) {
    (
        row.host.trim().to_lowercase(),
        row.username.trim().to_string(),
    )
}

/// Disabled rows hold no connections, but their cap still describes the account.
pub fn accounts(rows: &[UsenetServerRow]) -> Vec<AccountLoad> {
    let mut by_account: BTreeMap<(String, String), AccountLoad> = BTreeMap::new();
    for row in rows {
        let (host, username) = account(row);
        let load = by_account
            .entry((host.clone(), username.clone()))
            .or_insert(AccountLoad {
                host,
                username,
                connections: 0,
                cap: None,
            });
        if row.enabled {
            load.connections += i64::from(row.connections);
        }
        load.cap = match (load.cap, row.account_connection_cap) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }
    by_account.into_values().collect()
}

/// The account `candidate` belongs to once it replaces its stored row, if that is over cap.
pub fn violation(stored: &[UsenetServerRow], candidate: &UsenetServerRow) -> Option<AccountLoad> {
    let mut rows: Vec<UsenetServerRow> = stored
        .iter()
        .filter(|r| r.id != candidate.id)
        .cloned()
        .collect();
    rows.push(candidate.clone());
    let key = account(candidate);
    accounts(&rows)
        .into_iter()
        .find(|a| (a.host.clone(), a.username.clone()) == key)
        .filter(AccountLoad::over_cap)
}
