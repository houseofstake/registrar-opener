use near_sdk::borsh::{BorshDeserialize, BorshSerialize};
use near_sdk::serde_json;
use near_sdk::store::IterableMap;
use near_sdk::{env, near, require, AccountId, BorshStorageKey, NearToken, PanicOnDefault};

mod admin;
mod batch;
mod error;
mod names;
mod views;

#[cfg(test)]
mod tests;

use batch::Batch;

const STATE_VERSION: u16 = 1;

#[derive(BorshSerialize, BorshStorageKey)]
#[borsh(crate = "near_sdk::borsh")]
enum StorageKey {
    Batches,
    BatchNames { batch_id: u32 },
}

fn emit(event: &str, data: serde_json::Value) {
    env::log_str(&format!(
        r#"EVENT_JSON:{{"standard":"registrar_opener","version":"1.0.0","event":"{event}","data":[{data}]}}"#
    ));
}

fn assert_one_yocto() {
    require!(
        env::attached_deposit() == NearToken::from_yoctonear(1),
        error::ONE_YOCTO
    );
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct RegistrarOpener {
    state_version: u16,
    admin: AccountId,
    pending_admin: Option<AccountId>,
    operator: AccountId,
    batches: IterableMap<u32, Batch>,
    next_batch_id: u32,
    opened: u64,
    failed: u64,
}

#[near]
impl RegistrarOpener {
    #[init(ignore_state)]
    pub fn new(admin: AccountId, operator: AccountId) -> Self {
        let here = env::current_account_id();
        require!(env::predecessor_account_id() == here, error::ONLY_SELF);
        require!(
            env::storage_read(b"STATE")
                .and_then(|bytes| RegistrarOpener::try_from_slice(&bytes).ok())
                .is_none(),
            error::ALREADY_INSTALLED
        );
        require!(admin != operator, error::ADMIN_IS_OPERATOR);
        require!(admin != here && operator != here, error::ROLE_IS_SELF);
        emit(
            "installed",
            serde_json::json!({
                "state_version": STATE_VERSION,
                "admin": admin,
                "operator": operator,
            }),
        );
        Self {
            state_version: STATE_VERSION,
            admin,
            pending_admin: None,
            operator,
            batches: IterableMap::new(StorageKey::Batches),
            next_batch_id: 0,
            opened: 0,
            failed: 0,
        }
    }

    #[init(ignore_state)]
    pub fn migrate() -> Self {
        require!(
            env::predecessor_account_id() == env::current_account_id(),
            error::ONLY_SELF
        );
        let state: Self = env::state_read().unwrap_or_else(|| env::panic_str(error::NO_STATE));
        emit(
            "migrated",
            serde_json::json!({"state_version": state.state_version}),
        );
        state
    }
}

impl RegistrarOpener {
    fn assert_admin(&self) {
        assert_one_yocto();
        self.assert_admin_account();
    }

    fn assert_admin_account(&self) {
        require!(
            env::predecessor_account_id() == self.admin,
            error::ONLY_ADMIN
        );
    }

    fn assert_operator(&self) {
        assert_one_yocto();
        self.assert_operator_account();
    }

    fn assert_operator_account(&self) {
        require!(
            env::predecessor_account_id() == self.operator,
            error::ONLY_OPERATOR
        );
    }

    fn assert_admin_or_operator(&self) -> AccountId {
        assert_one_yocto();
        let caller = env::predecessor_account_id();
        require!(
            caller == self.operator || caller == self.admin,
            error::ONLY_ADMIN_OR_OPERATOR
        );
        caller
    }

    fn batch_mut(&mut self, batch_id: u32) -> &mut Batch {
        self.batches
            .get_mut(&batch_id)
            .unwrap_or_else(|| env::panic_str(error::NO_BATCH))
    }
}
