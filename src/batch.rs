use near_sdk::json_types::Base58CryptoHash;
use near_sdk::serde_json;
use near_sdk::store::IterableSet;
use near_sdk::utils::is_promise_success;
use near_sdk::{env, near, require, AccountId, CryptoHash, Gas, NearToken, Promise, PublicKey};

use crate::names::{assert_openable, fold_digest, seed_digest};
use crate::{emit, error, RegistrarOpener, RegistrarOpenerExt, StorageKey};

pub const GAS_FOR_CALLBACK: Gas = Gas::from_tgas(4);
pub const GAS_PER_NAME: Gas = Gas::from_tgas(14);
pub const MIN_FUNDING: NearToken = NearToken::from_millinear(10);
const MAX_NAMES_PER_CALL: usize = 20;
pub const MAX_NAMES_PER_ADD: usize = 100;
pub const MAX_NAMES_PER_BATCH: u32 = 600;
pub const MAX_LIVE_BATCHES: u32 = 4;

const _: () = assert!(
    GAS_PER_NAME.as_gas() * (MAX_NAMES_PER_CALL as u64) <= 300_000_000_000_000,
    "a full open call must fit the 300 Tgas a transaction can carry"
);
const _: () = assert!(
    GAS_FOR_CALLBACK.as_gas() < GAS_PER_NAME.as_gas(),
    "every name must budget more gas than its own callback spends"
);
const _: () = assert!(
    MAX_NAMES_PER_BATCH >= 528,
    "one batch must cover the launch cohort or the council votes twice"
);

#[near(serializers = [borsh])]
pub struct Batch {
    pub approved: bool,
    pub digest: CryptoHash,
    pub count: u32,
    pub owner_key: PublicKey,
    pub funding: NearToken,
    pub names: IterableSet<AccountId>,
}

#[near]
impl RegistrarOpener {
    pub fn create_batch(&mut self, owner_key: PublicKey, funding: NearToken) -> u32 {
        self.assert_operator();
        require!(funding >= MIN_FUNDING, error::FUNDING_TOO_LOW);
        require!(
            self.batches.len() < MAX_LIVE_BATCHES,
            error::TOO_MANY_BATCHES
        );
        let batch_id = self.next_batch_id;
        self.next_batch_id = batch_id
            .checked_add(1)
            .unwrap_or_else(|| env::panic_str(error::BATCH_IDS_EXHAUSTED));
        let batch = Batch {
            approved: false,
            digest: seed_digest(&owner_key, funding),
            count: 0,
            owner_key,
            funding,
            names: IterableSet::new(StorageKey::BatchNames { batch_id }),
        };
        self.batches.insert(batch_id, batch);
        emit(
            "batch_created",
            serde_json::json!({"batch_id": batch_id, "funding": funding}),
        );
        batch_id
    }

    pub fn add_names(&mut self, batch_id: u32, names: Vec<AccountId>) -> Base58CryptoHash {
        self.assert_operator();
        require!(names.len() <= MAX_NAMES_PER_ADD, error::TOO_MANY_NAMES);
        assert_openable(&names);
        let batch = self.batch_mut(batch_id);
        require!(!batch.approved, error::BATCH_APPROVED);
        require!(
            batch.count.saturating_add(names.len() as u32) <= MAX_NAMES_PER_BATCH,
            error::BATCH_FULL
        );
        for name in names {
            require!(batch.names.insert(name.clone()), error::DUPLICATE_NAME);
            batch.digest = fold_digest(batch.digest, &name);
            batch.count = batch.count.saturating_add(1);
        }
        let digest = Base58CryptoHash::from(batch.digest);
        let count = batch.count;
        emit(
            "names_added",
            serde_json::json!({"batch_id": batch_id, "count": count, "digest": digest}),
        );
        digest
    }

    pub fn discard_batch(&mut self, batch_id: u32) {
        let caller = env::predecessor_account_id();
        require!(
            caller == self.operator || caller == self.admin,
            error::ONLY_ADMIN_OR_OPERATOR
        );
        let revoking = caller == self.admin;
        let batch = self.batch_mut(batch_id);
        let spent = batch.names.is_empty();
        require!(revoking || spent || !batch.approved, error::BATCH_LIVE);
        batch.names.clear();
        self.batches.remove(&batch_id);
        emit(
            "batch_discarded",
            serde_json::json!({"batch_id": batch_id, "revoked": revoking}),
        );
    }

    #[payable]
    pub fn open_names(&mut self, batch_id: u32, names: Vec<AccountId>) -> u32 {
        self.assert_operator();
        require!(names.len() <= MAX_NAMES_PER_CALL, error::TOO_MANY_NAMES);
        assert_openable(&names);
        let needed = GAS_PER_NAME.as_gas().saturating_mul(names.len() as u64);
        require!(env::prepaid_gas().as_gas() >= needed, error::GAS_TOO_LOW);
        let here = env::current_account_id();
        let batch = self.batch_mut(batch_id);
        require!(batch.approved, error::BATCH_NOT_APPROVED);
        let funding = batch.funding;
        let owner_key = batch.owner_key.clone();
        let total = funding.as_yoctonear().saturating_mul(names.len() as u128);
        require!(
            env::attached_deposit() == NearToken::from_yoctonear(total),
            error::DEPOSIT_MISMATCH
        );
        for name in &names {
            require!(batch.names.remove(name), error::NOT_IN_BATCH);
        }
        for name in &names {
            emit(
                "opening",
                serde_json::json!({"name": name, "batch_id": batch_id}),
            );
            Promise::new(name.clone())
                .create_account()
                .transfer(funding)
                .add_full_access_key(owner_key.clone())
                .then(
                    Self::ext(here.clone())
                        .with_static_gas(GAS_FOR_CALLBACK)
                        .on_name_opened(Some(batch_id), Some(name.clone())),
                )
                .detach();
        }
        names.len() as u32
    }

    #[private]
    pub fn on_name_opened(&mut self, batch_id: Option<u32>, name: Option<AccountId>) -> bool {
        if is_promise_success() {
            self.opened = self.opened.saturating_add(1);
            emit(
                "opened",
                serde_json::json!({"batch_id": batch_id, "name": name}),
            );
            return true;
        }
        self.failed = self.failed.saturating_add(1);
        if let (Some(batch_id), Some(name)) = (batch_id, name.clone()) {
            if let Some(batch) = self.batches.get_mut(&batch_id) {
                batch.names.insert(name);
            }
        }
        emit(
            "open_failed",
            serde_json::json!({"batch_id": batch_id, "name": name}),
        );
        false
    }
}
