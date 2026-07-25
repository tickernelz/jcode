use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{LazyLock, RwLock};

pub struct CrossProcessFileLock(std::fs::File);

impl CrossProcessFileLock {
    pub fn acquire(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        file.lock()?;
        Ok(Self(file))
    }
}

impl Drop for CrossProcessFileLock {
    fn drop(&mut self) {
        if let Err(error) = self.0.unlock() {
            crate::logging::warn(&format!("Failed to release auth file lock: {error}"));
        }
    }
}

static RUNTIME_CREDENTIAL_IDENTITIES: LazyLock<RwLock<HashMap<String, StoredAccountIdentity>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Return an opaque, non-secret identity for a credential source that has no
/// provider account store of its own. The generation is deliberately renewed
/// once per process: ambient API keys and cloud credential chains cannot be
/// compared without inspecting secret material, so cross-process resumability
/// must fail closed while identities remain stable within the live runtime.
pub fn runtime_credential_identity(key: &str) -> (String, String, u64) {
    let mut identities = RUNTIME_CREDENTIAL_IDENTITIES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let identity = identities
        .entry(key.to_string())
        .or_insert_with(|| StoredAccountIdentity {
            id: crate::id::new_id("credential"),
            generation: 1,
        });
    (key.to_string(), identity.id.clone(), identity.generation)
}

pub fn invalidate_runtime_credential_identities() {
    let mut identities = RUNTIME_CREDENTIAL_IDENTITIES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for identity in identities.values_mut() {
        identity.generation = identity.generation.saturating_add(1);
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct StoredAccountIdentity {
    pub id: String,
    #[serde(default = "default_account_generation")]
    pub generation: u64,
}

fn default_account_generation() -> u64 {
    1
}

pub fn ensure_account_identities<'a>(
    labels: impl IntoIterator<Item = &'a str>,
    identities: &mut HashMap<String, StoredAccountIdentity>,
) -> bool {
    let labels = labels.into_iter().collect::<std::collections::HashSet<_>>();
    let before = identities.len();
    identities.retain(|label, _| labels.contains(label.as_str()));
    let mut changed = identities.len() != before;
    for label in labels {
        if !identities.contains_key(label) {
            identities.insert(
                label.to_string(),
                StoredAccountIdentity {
                    id: crate::id::new_id("account"),
                    generation: 1,
                },
            );
            changed = true;
        }
    }
    changed
}

/// Runtime (process-local) active-account overrides, keyed by provider
/// prefix ("claude", "openai", ...). Lets `/account switch <label>` take
/// effect immediately without rewriting the provider auth file.
///
/// Centralized here so every provider shares one mechanism instead of
/// duplicating a `static ACTIVE_ACCOUNT_OVERRIDE` per module.
static RUNTIME_ACTIVE_OVERRIDES: LazyLock<RwLock<HashMap<&'static str, String>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

pub fn set_runtime_active_override(prefix: &'static str, label: Option<String>) {
    if let Ok(mut overrides) = RUNTIME_ACTIVE_OVERRIDES.write() {
        match label {
            Some(label) => {
                overrides.insert(prefix, label);
            }
            None => {
                overrides.remove(prefix);
            }
        }
    }
}

pub fn runtime_active_override(prefix: &str) -> Option<String> {
    RUNTIME_ACTIVE_OVERRIDES
        .read()
        .ok()
        .and_then(|overrides| overrides.get(prefix).cloned())
}

pub fn canonical_account_label(prefix: &str, index: usize) -> String {
    format!("{prefix}-{index}")
}

pub fn next_account_label(prefix: &str, account_count: usize) -> String {
    canonical_account_label(prefix, account_count + 1)
}

pub fn login_target_label<T, F>(
    prefix: &str,
    requested: Option<&str>,
    active_label: Option<String>,
    accounts: &[T],
    label_of: F,
) -> String
where
    F: Fn(&T) -> &str + Copy,
{
    if let Some(requested) = requested
        .map(str::trim)
        .filter(|requested| !requested.is_empty())
    {
        if accounts
            .iter()
            .any(|account| label_of(account) == requested)
        {
            return requested.to_string();
        }
        return next_account_label(prefix, accounts.len());
    }

    active_label
        .or_else(|| {
            accounts
                .first()
                .map(|account| label_of(account).to_string())
        })
        .unwrap_or_else(|| canonical_account_label(prefix, 1))
}

pub fn active_account_label<T, F>(
    override_label: Option<String>,
    stored_active_label: Option<String>,
    accounts: &[T],
    label_of: F,
) -> Option<String>
where
    F: Fn(&T) -> &str + Copy,
{
    override_label.or(stored_active_label).or_else(|| {
        accounts
            .first()
            .map(|account| label_of(account).to_string())
    })
}

pub fn set_active_account<T, F>(
    label: &str,
    accounts: &[T],
    stored_active_label: &mut Option<String>,
    missing_message: &str,
    label_of: F,
) -> Result<()>
where
    F: Fn(&T) -> &str + Copy,
{
    if !accounts.iter().any(|account| label_of(account) == label) {
        anyhow::bail!(missing_message.replace("{}", label));
    }
    *stored_active_label = Some(label.to_string());
    Ok(())
}

pub fn upsert_account<T, FGet, FSet>(
    prefix: &str,
    accounts: &mut Vec<T>,
    stored_active_label: &mut Option<String>,
    account: T,
    label_of: FGet,
    set_label: FSet,
) -> String
where
    FGet: Fn(&T) -> &str + Copy,
    FSet: Fn(&mut T, String) + Copy,
{
    let requested_label = label_of(&account).to_string();
    if let Some(existing) = accounts
        .iter_mut()
        .find(|existing| label_of(existing) == requested_label)
    {
        *existing = account;
        return requested_label;
    }

    let label = next_account_label(prefix, accounts.len());
    let mut account = account;
    set_label(&mut account, label.clone());
    accounts.push(account);

    if stored_active_label.is_none() || accounts.len() == 1 {
        *stored_active_label = Some(label.clone());
    }

    label
}

pub struct RelabelOutcome {
    pub changed: bool,
    pub canonical_override_label: Option<String>,
}

pub fn relabel_accounts<T, FGet, FSet>(
    prefix: &str,
    accounts: &mut [T],
    stored_active_label: &mut Option<String>,
    override_label: Option<String>,
    label_of: FGet,
    set_label: FSet,
) -> RelabelOutcome
where
    FGet: Fn(&T) -> &str + Copy,
    FSet: Fn(&mut T, String) + Copy,
{
    let label_map = accounts
        .iter()
        .enumerate()
        .map(|(index, account)| {
            (
                label_of(account).to_string(),
                canonical_account_label(prefix, index + 1),
            )
        })
        .collect::<Vec<_>>();
    let mut changed = false;

    for (account, (_, canonical_label)) in accounts.iter_mut().zip(label_map.iter()) {
        if label_of(account) != canonical_label {
            set_label(account, canonical_label.clone());
            changed = true;
        }
    }

    let desired_active = if accounts.is_empty() {
        None
    } else {
        stored_active_label
            .as_deref()
            .and_then(|label| {
                label_map
                    .iter()
                    .find(|(original, _)| original == label)
                    .map(|(_, canonical)| canonical.clone())
            })
            .or_else(|| {
                accounts
                    .first()
                    .map(|account| label_of(account).to_string())
            })
    };

    if *stored_active_label != desired_active {
        *stored_active_label = desired_active;
        changed = true;
    }

    let canonical_override_label = override_label.and_then(|override_label| {
        label_map
            .iter()
            .find(|(original, _)| original == &override_label)
            .and_then(|(_, canonical)| (override_label != *canonical).then(|| canonical.clone()))
    });

    RelabelOutcome {
        changed,
        canonical_override_label,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct Account {
        label: String,
    }

    #[test]
    fn relabel_accounts_canonicalizes_labels_and_active_label() {
        let mut accounts = vec![
            Account {
                label: "default".to_string(),
            },
            Account {
                label: "other".to_string(),
            },
        ];
        let mut active = Some("other".to_string());

        let outcome = relabel_accounts(
            "openai",
            &mut accounts,
            &mut active,
            Some("default".to_string()),
            |account| account.label.as_str(),
            |account, label| account.label = label,
        );

        assert!(outcome.changed);
        assert_eq!(accounts[0].label, "openai-1");
        assert_eq!(accounts[1].label, "openai-2");
        assert_eq!(active.as_deref(), Some("openai-2"));
        assert_eq!(
            outcome.canonical_override_label.as_deref(),
            Some("openai-1")
        );
    }

    #[test]
    fn upsert_account_assigns_next_label_and_sets_initial_active() {
        let mut accounts = Vec::<Account>::new();
        let mut active = None;

        let label = upsert_account(
            "claude",
            &mut accounts,
            &mut active,
            Account {
                label: "ignored".to_string(),
            },
            |account| account.label.as_str(),
            |account, label| account.label = label,
        );

        assert_eq!(label, "claude-1");
        assert_eq!(accounts[0].label, "claude-1");
        assert_eq!(active.as_deref(), Some("claude-1"));
    }
}
