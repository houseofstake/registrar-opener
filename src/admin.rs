use near_sdk::json_types::{Base58CryptoHash, Base64VecU8};
use near_sdk::serde_json;
use near_sdk::utils::is_promise_success;
use near_sdk::{env, near, require, AccountId, CryptoHash, Gas, NearToken, Promise, PublicKey};

use crate::batch::{GAS_FOR_CALLBACK, MIN_FUNDING};
use crate::names::{assert_openable, to_hash};
use crate::{assert_one_yocto, emit, error, RegistrarOpener, RegistrarOpenerExt};

const GAS_FOR_MIGRATE: Gas = Gas::from_tgas(30);
const GAS_FOR_UPGRADE_CALLBACK: Gas = Gas::from_tgas(5);

#[near]
impl RegistrarOpener {
    #[payable]
    pub fn approve_batch(&mut self, batch_id: u32, digest: Base58CryptoHash) {
        self.assert_admin();
        let batch = self.batch_mut(batch_id);
        require!(!batch.approved, error::BATCH_APPROVED);
        require!(batch.remaining > 0, error::BATCH_EMPTY);
        require!(
            batch.digest == CryptoHash::from(digest),
            error::DIGEST_MISMATCH
        );
        batch.approved = true;
        let count = batch.count;
        emit(
            "batch_approved",
            serde_json::json!({"batch_id": batch_id, "count": count, "digest": digest}),
        );
    }

    #[payable]
    pub fn change_operator(&mut self, operator: AccountId) {
        self.assert_admin();
        require!(operator != self.admin, error::ADMIN_IS_OPERATOR);
        require!(
            self.pending_admin.as_ref() != Some(&operator),
            error::NOMINEE_IS_OPERATOR
        );
        require!(operator != env::current_account_id(), error::ROLE_IS_SELF);
        self.operator = operator.clone();
        emit(
            "operator_changed",
            serde_json::json!({"operator": operator}),
        );
    }

    #[payable]
    pub fn change_admin(&mut self, admin: AccountId) {
        self.assert_admin();
        require!(admin != self.operator, error::ADMIN_IS_OPERATOR);
        require!(admin != env::current_account_id(), error::ROLE_IS_SELF);
        self.pending_admin = Some(admin.clone());
        emit("admin_nominated", serde_json::json!({"admin": admin}));
    }

    #[payable]
    pub fn accept_admin(&mut self) {
        assert_one_yocto();
        let nominee = self
            .pending_admin
            .clone()
            .unwrap_or_else(|| env::panic_str(error::NO_PENDING_ADMIN));
        require!(
            env::predecessor_account_id() == nominee,
            error::ONLY_PENDING_ADMIN
        );
        require!(nominee != self.operator, error::NOMINEE_IS_OPERATOR);
        self.admin = nominee.clone();
        self.pending_admin = None;
        emit("admin_changed", serde_json::json!({"admin": nominee}));
    }

    #[payable]
    pub fn cancel_nomination(&mut self) {
        self.assert_admin();
        let nominee = self
            .pending_admin
            .take()
            .unwrap_or_else(|| env::panic_str(error::NO_PENDING_ADMIN));
        emit(
            "nomination_cancelled",
            serde_json::json!({"admin": nominee}),
        );
    }

    #[payable]
    pub fn create_account(&mut self, name: AccountId, owner_key: PublicKey) -> Promise {
        self.assert_admin_account();
        let funding = env::attached_deposit();
        require!(funding >= MIN_FUNDING, error::FUNDING_TOO_LOW);
        assert_openable(std::slice::from_ref(&name));
        emit(
            "opening",
            serde_json::json!({"name": name, "batch_id": Option::<u32>::None}),
        );
        Promise::new(name.clone())
            .create_account()
            .transfer(funding)
            .add_full_access_key(owner_key)
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(GAS_FOR_CALLBACK)
                    .on_account_created(name),
            )
    }

    #[private]
    pub fn on_account_created(&mut self, name: AccountId) {
        require!(is_promise_success(), error::OPEN_FAILED);
        self.opened = self.opened.saturating_add(1);
        emit(
            "opened",
            serde_json::json!({"batch_id": Option::<u32>::None, "name": name}),
        );
    }

    #[payable]
    pub fn upgrade(&mut self, code: Base64VecU8) -> Promise {
        self.assert_admin();
        let here = env::current_account_id();
        let code_hash = Base58CryptoHash::from(to_hash(env::sha256(&code.0)));
        emit("upgrading", serde_json::json!({"code_hash": code_hash}));
        Promise::new(here.clone())
            .deploy_contract(code.0)
            .function_call(
                "migrate".to_string(),
                Vec::new(),
                NearToken::from_yoctonear(0),
                GAS_FOR_MIGRATE,
            )
            .then(
                Self::ext(here)
                    .with_static_gas(GAS_FOR_UPGRADE_CALLBACK)
                    .on_upgraded(code_hash),
            )
    }

    #[private]
    pub fn on_upgraded(&mut self, code_hash: Base58CryptoHash) {
        require!(is_promise_success(), error::UPGRADE_FAILED);
        emit("upgraded", serde_json::json!({"code_hash": code_hash}));
    }
}
