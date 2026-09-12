use near_sdk::borsh::{BorshDeserialize, BorshSerialize};
use near_sdk::json_types::{Base58CryptoHash, Base64VecU8, U64};
use near_sdk::serde_json;
use near_sdk::store::{IterableMap, IterableSet};
use near_sdk::utils::is_promise_success;
use near_sdk::{
    env, near, require, AccountId, BorshStorageKey, CryptoHash, Gas, NearToken, PanicOnDefault,
    Promise, PublicKey,
};

#[cfg(test)]
mod tests;

const STATE_VERSION: u16 = 1;
const MAINNET_UPGRADE_DELAY_NS: u64 = 48 * 60 * 60 * 1_000_000_000;
const DIGEST_DOMAIN: &[u8] = b"registrar-opener:batch:v1";
const GAS_FOR_CALLBACK: Gas = Gas::from_tgas(4);
const GAS_PER_NAME: Gas = Gas::from_tgas(14);
const GAS_FOR_MIGRATE: Gas = Gas::from_tgas(30);
const GAS_FOR_UPGRADE_CALLBACK: Gas = Gas::from_tgas(5);
const MAX_NAMES_PER_CALL: usize = 20;
const MAX_NAMES_PER_ADD: usize = 100;
const MAX_NAMES_PER_BATCH: u32 = 600;
const MAX_LIVE_BATCHES: u32 = 4;
const MIN_TLA_LEN: usize = 3;
const MAX_TLA_LEN: usize = 64;
const MIN_FUNDING: NearToken = NearToken::from_millinear(10);
const LIST_PAGE_LIMIT: u32 = 200;

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
const _: () = assert!(
    MIN_TLA_LEN >= 3,
    "two character top level names are cheap to forge against light clients"
);
const _: () = assert!(
    MAX_TLA_LEN <= 64,
    "the protocol refuses account ids longer than 64 bytes"
);

mod error {
    pub const ONLY_ADMIN: &str = "only the admin may call this";
    pub const ONLY_OPERATOR: &str = "only the current operator may call this";
    pub const ONLY_ADMIN_OR_OPERATOR: &str = "only the admin or the operator may call this";
    pub const ONLY_SELF: &str = "only this account may call this";
    pub const ONLY_PENDING_ADMIN: &str = "only the nominated admin may accept";
    pub const NO_PENDING_ADMIN: &str = "no admin has been nominated";
    pub const ADMIN_IS_OPERATOR: &str = "admin and operator must be different accounts";
    pub const ROLE_IS_SELF: &str = "neither role may be this account";
    pub const INSTANT_UPGRADE: &str = "the upgrade delay must be greater than zero";
    pub const ALREADY_INSTALLED: &str = "this account already runs the opener";
    pub const NO_BATCH: &str = "no batch with that id";
    pub const BATCH_APPROVED: &str = "the batch is approved and can no longer be edited";
    pub const BATCH_NOT_APPROVED: &str = "the batch has not been approved";
    pub const BATCH_STALE: &str = "the batch belongs to a replaced operator";
    pub const BATCH_EMPTY: &str = "the batch holds no names";
    pub const BATCH_FULL: &str = "the batch is at the per batch name limit";
    pub const BATCH_LIVE: &str = "an approved batch still holding names cannot be discarded";
    pub const DIGEST_MISMATCH: &str = "digest does not match the batch contents";
    pub const NOT_IN_BATCH: &str = "name is not in this batch";
    pub const EMPTY_NAMES: &str = "no names supplied";
    pub const TOO_MANY_NAMES: &str = "more names than this call accepts";
    pub const DUPLICATE_NAME: &str = "duplicate name in list";
    pub const NOT_TOP_LEVEL: &str = "name is not a top level account";
    pub const NAME_TOO_SHORT: &str = "top level name is short enough to be forgeable";
    pub const NAME_TOO_LONG: &str = "name exceeds the account id limit";
    pub const NAME_IS_IMPLICIT: &str = "an implicit account id is somebody's derived address";
    pub const TOO_MANY_BATCHES: &str = "discard a batch before drafting another";
    pub const BATCH_IDS_EXHAUSTED: &str = "batch ids are exhausted, reusing one would alias its \
                                           stored names";
    pub const GAS_TOO_LOW: &str = "attach more gas or send fewer names";
    pub const FUNDING_TOO_LOW: &str = "funding is below the account storage floor";
    pub const DEPOSIT_MISMATCH: &str = "attached deposit must be the funding times the name count";
    pub const NO_PENDING_CODE: &str = "no code hash has been approved";
    pub const CODE_MISMATCH: &str = "this code does not match the approved hash";
    pub const UPGRADE_TOO_EARLY: &str = "the approval delay has not elapsed";
    pub const UPGRADE_FAILED: &str = "the deploy did not land, the approval still stands";
    pub const ONE_YOCTO: &str = "exactly one yoctoNEAR must be attached";
    pub const NO_STATE: &str = "there is no state to migrate";
}

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

fn to_hash(bytes: Vec<u8>) -> CryptoHash {
    let mut hash = CryptoHash::default();
    hash.copy_from_slice(&bytes);
    hash
}

fn seed_digest(owner_key: &PublicKey, funding: NearToken) -> CryptoHash {
    let mut bytes = DIGEST_DOMAIN.to_vec();
    bytes.extend_from_slice(owner_key.as_bytes());
    bytes.extend_from_slice(&funding.as_yoctonear().to_le_bytes());
    to_hash(env::sha256(&bytes))
}

fn fold_digest(digest: CryptoHash, name: &AccountId) -> CryptoHash {
    let mut bytes = digest.to_vec();
    bytes.extend_from_slice(name.as_bytes());
    to_hash(env::sha256(&bytes))
}

fn assert_one_yocto() {
    require!(
        env::attached_deposit() == NearToken::from_yoctonear(1),
        error::ONE_YOCTO
    );
}

fn assert_names_openable(names: &[AccountId]) {
    require!(!names.is_empty(), error::EMPTY_NAMES);
    let mut seen: Vec<&str> = names.iter().map(|name| name.as_str()).collect();
    seen.sort_unstable();
    let before = seen.len();
    seen.dedup();
    require!(seen.len() == before, error::DUPLICATE_NAME);
    for name in names {
        let raw = name.as_str();
        require!(!raw.contains('.'), error::NOT_TOP_LEVEL);
        require!(raw.len() >= MIN_TLA_LEN, error::NAME_TOO_SHORT);
        require!(raw.len() <= MAX_TLA_LEN, error::NAME_TOO_LONG);
        require!(
            !name.get_account_type().is_implicit(),
            error::NAME_IS_IMPLICIT
        );
    }
}

#[near(serializers = [borsh])]
pub struct Batch {
    operator: AccountId,
    operator_epoch: u32,
    approved: bool,
    digest: CryptoHash,
    count: u32,
    owner_key: PublicKey,
    funding: NearToken,
    names: IterableSet<AccountId>,
}

#[near(serializers = [json])]
pub struct BatchView {
    pub operator: AccountId,
    pub operator_epoch: u32,
    pub approved: bool,
    pub stale: bool,
    pub digest: Base58CryptoHash,
    pub count: u32,
    pub remaining: u32,
    pub owner_key: PublicKey,
    pub funding: NearToken,
}

#[near(serializers = [borsh, json])]
#[derive(Clone)]
pub struct PendingCode {
    pub code_hash: Base58CryptoHash,
    pub earliest_at_ns: U64,
}

#[near(serializers = [json])]
pub struct OpenerView {
    pub state_version: u16,
    pub admin: AccountId,
    pub pending_admin: Option<AccountId>,
    pub operator: AccountId,
    pub operator_epoch: u32,
    pub upgrade_delay_ns: U64,
    pub mainnet_upgrade_delay_ns: U64,
    pub next_batch_id: u32,
    pub pending_code: Option<PendingCode>,
    pub opened: u64,
    pub failed: u64,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct RegistrarOpener {
    state_version: u16,
    admin: AccountId,
    pending_admin: Option<AccountId>,
    operator: AccountId,
    operator_epoch: u32,
    upgrade_delay_ns: u64,
    batches: IterableMap<u32, Batch>,
    next_batch_id: u32,
    pending_code: Option<PendingCode>,
    opened: u64,
    failed: u64,
}

#[near]
impl RegistrarOpener {
    #[init(ignore_state)]
    pub fn new(admin: AccountId, operator: AccountId, upgrade_delay_ns: U64) -> Self {
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
        require!(upgrade_delay_ns.0 > 0, error::INSTANT_UPGRADE);
        emit(
            "installed",
            serde_json::json!({
                "state_version": STATE_VERSION,
                "admin": admin,
                "operator": operator,
                "upgrade_delay_ns": upgrade_delay_ns,
            }),
        );
        Self {
            state_version: STATE_VERSION,
            admin,
            pending_admin: None,
            operator,
            operator_epoch: 0,
            upgrade_delay_ns: upgrade_delay_ns.0,
            batches: IterableMap::new(StorageKey::Batches),
            next_batch_id: 0,
            pending_code: None,
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

#[near]
impl RegistrarOpener {
    #[payable]
    pub fn approve_batch(&mut self, batch_id: u32, digest: Base58CryptoHash) {
        self.assert_admin();
        let operator = self.operator.clone();
        let epoch = self.operator_epoch;
        let batch = self.batch_mut(batch_id);
        require!(!batch.approved, error::BATCH_APPROVED);
        require!(
            batch.operator == operator && batch.operator_epoch == epoch,
            error::BATCH_STALE
        );
        require!(!batch.names.is_empty(), error::BATCH_EMPTY);
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
        require!(operator != env::current_account_id(), error::ROLE_IS_SELF);
        self.operator = operator.clone();
        self.operator_epoch = self.operator_epoch.saturating_add(1);
        let operator_epoch = self.operator_epoch;
        emit(
            "operator_changed",
            serde_json::json!({"operator": operator, "operator_epoch": operator_epoch}),
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
        self.admin = nominee.clone();
        self.pending_admin = None;
        emit("admin_changed", serde_json::json!({"admin": nominee}));
    }

    #[payable]
    pub fn create_account(&mut self, name: AccountId, owner_key: PublicKey) -> Promise {
        self.assert_admin_account();
        let funding = env::attached_deposit();
        require!(funding >= MIN_FUNDING, error::FUNDING_TOO_LOW);
        assert_names_openable(std::slice::from_ref(&name));
        emit(
            "opening",
            serde_json::json!({"name": name, "batch_id": Option::<u32>::None}),
        );
        Promise::new(name)
            .create_account()
            .transfer(funding)
            .add_full_access_key(owner_key)
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(GAS_FOR_CALLBACK)
                    .on_name_opened(None, self.operator_epoch, None),
            )
    }

    #[payable]
    pub fn approve_code(&mut self, code_hash: Base58CryptoHash) {
        self.assert_admin();
        let earliest_at_ns = U64(env::block_timestamp().saturating_add(self.upgrade_delay_ns));
        self.pending_code = Some(PendingCode {
            code_hash,
            earliest_at_ns,
        });
        emit(
            "code_approved",
            serde_json::json!({"code_hash": code_hash, "earliest_at_ns": earliest_at_ns}),
        );
    }

    #[payable]
    pub fn cancel_code(&mut self) {
        self.assert_admin();
        self.pending_code = None;
        emit("code_canceled", serde_json::json!({}));
    }

    pub fn upgrade(&mut self, code: Base64VecU8) -> Promise {
        let caller = env::predecessor_account_id();
        require!(
            caller == self.admin || caller == self.operator,
            error::ONLY_ADMIN_OR_OPERATOR
        );
        let pending = self
            .pending_code
            .clone()
            .unwrap_or_else(|| env::panic_str(error::NO_PENDING_CODE));
        require!(
            env::block_timestamp() >= pending.earliest_at_ns.0,
            error::UPGRADE_TOO_EARLY
        );
        require!(
            to_hash(env::sha256(&code.0)) == CryptoHash::from(pending.code_hash),
            error::CODE_MISMATCH
        );
        let here = env::current_account_id();
        emit(
            "upgrading",
            serde_json::json!({"code_hash": pending.code_hash}),
        );
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
                    .on_upgraded(pending.code_hash),
            )
    }

    #[private]
    pub fn on_upgraded(&mut self, code_hash: Base58CryptoHash) {
        require!(is_promise_success(), error::UPGRADE_FAILED);
        if self
            .pending_code
            .as_ref()
            .is_some_and(|pending| pending.code_hash == code_hash)
        {
            self.pending_code = None;
        }
        emit("upgraded", serde_json::json!({"code_hash": code_hash}));
    }
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
        self.next_batch_id = batch_id.checked_add(1).unwrap_or_else(|| {
            env::panic_str(error::BATCH_IDS_EXHAUSTED);
        });
        let batch = Batch {
            operator: self.operator.clone(),
            operator_epoch: self.operator_epoch,
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
        assert_names_openable(&names);
        let operator = self.operator.clone();
        let epoch = self.operator_epoch;
        let batch = self.batch_mut(batch_id);
        require!(!batch.approved, error::BATCH_APPROVED);
        require!(
            batch.operator == operator && batch.operator_epoch == epoch,
            error::BATCH_STALE
        );
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
        let epoch = self.operator_epoch;
        let batch = self.batch_mut(batch_id);
        let stale = batch.operator_epoch != epoch;
        let spent = batch.names.is_empty();
        require!(
            revoking || stale || spent || !batch.approved,
            error::BATCH_LIVE
        );
        batch.names.clear();
        self.batches.remove(&batch_id);
        emit(
            "batch_discarded",
            serde_json::json!({"batch_id": batch_id, "stale": stale, "revoked": revoking}),
        );
    }

    #[payable]
    pub fn open_names(&mut self, batch_id: u32, names: Vec<AccountId>) -> u32 {
        self.assert_operator();
        require!(names.len() <= MAX_NAMES_PER_CALL, error::TOO_MANY_NAMES);
        assert_names_openable(&names);
        let needed = GAS_PER_NAME.as_gas().saturating_mul(names.len() as u64);
        require!(env::prepaid_gas().as_gas() >= needed, error::GAS_TOO_LOW);
        let operator = self.operator.clone();
        let epoch = self.operator_epoch;
        let here = env::current_account_id();
        let batch = self.batch_mut(batch_id);
        require!(batch.approved, error::BATCH_NOT_APPROVED);
        require!(
            batch.operator == operator && batch.operator_epoch == epoch,
            error::BATCH_STALE
        );
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
                        .on_name_opened(Some(batch_id), epoch, Some(name.clone())),
                )
                .detach();
        }
        names.len() as u32
    }

    #[private]
    pub fn on_name_opened(
        &mut self,
        batch_id: Option<u32>,
        operator_epoch: u32,
        name: Option<AccountId>,
    ) -> bool {
        if is_promise_success() {
            self.opened = self.opened.saturating_add(1);
            emit(
                "opened",
                serde_json::json!({"batch_id": batch_id, "name": name}),
            );
            return true;
        }
        self.failed = self.failed.saturating_add(1);
        let live = operator_epoch == self.operator_epoch;
        if let (true, Some(batch_id), Some(name)) = (live, batch_id, name.clone()) {
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

#[near]
impl RegistrarOpener {
    pub fn get_batch(&self, batch_id: u32) -> Option<BatchView> {
        self.batches.get(&batch_id).map(|batch| BatchView {
            operator: batch.operator.clone(),
            operator_epoch: batch.operator_epoch,
            approved: batch.approved,
            stale: batch.operator_epoch != self.operator_epoch,
            digest: Base58CryptoHash::from(batch.digest),
            count: batch.count,
            remaining: batch.names.len(),
            owner_key: batch.owner_key.clone(),
            funding: batch.funding,
        })
    }

    pub fn list_batch_names(
        &self,
        batch_id: u32,
        from_index: Option<u32>,
        limit: Option<u32>,
    ) -> Vec<AccountId> {
        let batch = match self.batches.get(&batch_id) {
            Some(batch) => batch,
            None => return Vec::new(),
        };
        let take = limit.unwrap_or(LIST_PAGE_LIMIT).min(LIST_PAGE_LIMIT) as usize;
        batch
            .names
            .iter()
            .skip(from_index.unwrap_or(0) as usize)
            .take(take)
            .cloned()
            .collect()
    }

    pub fn opener_view(&self) -> OpenerView {
        OpenerView {
            state_version: self.state_version,
            admin: self.admin.clone(),
            pending_admin: self.pending_admin.clone(),
            operator: self.operator.clone(),
            operator_epoch: self.operator_epoch,
            upgrade_delay_ns: U64(self.upgrade_delay_ns),
            mainnet_upgrade_delay_ns: U64(MAINNET_UPGRADE_DELAY_NS),
            next_batch_id: self.next_batch_id,
            pending_code: self.pending_code.clone(),
            opened: self.opened,
            failed: self.failed,
        }
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
        require!(
            env::predecessor_account_id() == self.operator,
            error::ONLY_OPERATOR
        );
    }

    fn batch_mut(&mut self, batch_id: u32) -> &mut Batch {
        self.batches
            .get_mut(&batch_id)
            .unwrap_or_else(|| env::panic_str(error::NO_BATCH))
    }
}
