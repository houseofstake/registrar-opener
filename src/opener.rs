use near_sdk::borsh::{self, BorshDeserialize, BorshSerialize};
use near_sdk::collections::LookupMap;
use near_sdk::is_promise_success;
use near_sdk::json_types::{U128, U64};
use near_sdk::serde::{Deserialize, Serialize};
use near_sdk::{env, ext_contract, near_bindgen, AccountId, Balance, Gas, Promise, PublicKey};

use crate::MultiSigContract;
#[cfg(not(target_arch = "wasm32"))]
use crate::MultiSigContractContract;

const GRANT_PREFIX: &[u8] = b"o:g:";
const EPOCH_PREFIX: &[u8] = b"o:e:";
const APPROVED_PREFIX: &[u8] = b"o:n";
const OPENED_KEY: &[u8] = b"o:opened";
const FAILED_KEY: &[u8] = b"o:failed";

const GAS_FOR_CALLBACK: Gas = Gas(4_000_000_000_000);
const GAS_PER_NAME: Gas = Gas(14_000_000_000_000);
const MAX_NAMES_PER_CALL: usize = 20;
const MAX_NAMES_PER_GRANT: usize = 600;
const MIN_TLA_LEN: usize = 3;
const MAX_TLA_LEN: usize = 64;
const MIN_FUNDING: Balance = 10_000_000_000_000_000_000_000;
const _: () = assert!(
    GAS_PER_NAME.0 * (MAX_NAMES_PER_CALL as u64) <= 300_000_000_000_000,
    "a full batch must fit the 300 Tgas a transaction can carry"
);
const _: () = assert!(
    GAS_FOR_CALLBACK.0 < GAS_PER_NAME.0,
    "every name must budget more gas than its own callback spends"
);
const _: () = assert!(
    MAX_NAMES_PER_GRANT >= 528,
    "one grant must cover the 528 name launch cohort or the council votes twice"
);
const _: () = assert!(MIN_TLA_LEN >= 3, "two character names are cheap to forge");
const _: () = assert!(
    MAX_TLA_LEN <= 64,
    "the protocol refuses account ids longer than 64 bytes"
);
const _: () = assert!(
    MIN_FUNDING > 0,
    "an account cannot be opened with no balance"
);

mod error {
    pub const ONLY_SELF: &str = "only a confirmed multisig request may call this";
    pub const NO_GRANT: &str = "caller holds no grant";
    pub const GRANT_EXPIRED: &str = "grant has expired";
    pub const GRANT_LIVE: &str = "grantee already holds an unspent grant, revoke it first";
    pub const NOT_APPROVED: &str = "name is not in this grantee's approved list";
    pub const GRANT_EXHAUSTED: &str = "grant does not cover this many names";
    pub const EMPTY_BATCH: &str = "no names supplied";
    pub const BATCH_TOO_LARGE: &str = "batch exceeds the per-call limit";
    pub const GRANT_TOO_LARGE: &str = "grant exceeds the per-grant name limit";
    pub const DUPLICATE_NAME: &str = "duplicate name in list";
    pub const NOT_TOP_LEVEL: &str = "name is not a top level account";
    pub const NAME_TOO_SHORT: &str = "top level name is short enough to be forgeable";
    pub const NAME_TOO_LONG: &str = "name exceeds the account id limit";
    pub const GAS_TOO_LOW: &str = "attach more gas or send fewer names";
    pub const BALANCE_TOO_LOW: &str = "account balance does not cover the funding";
    pub const FUNDING_TOO_LOW: &str = "funding is below the account storage floor";
    pub const EXPIRY_IN_PAST: &str = "grant expiry is already past";
    pub const MULTISIG_UNSOUND: &str =
        "the multisig state did not survive the upgrade, refusing every opener operation";
}

#[derive(BorshDeserialize, BorshSerialize, Serialize, Deserialize, Clone)]
#[serde(crate = "near_sdk::serde")]
pub struct Grant {
    pub epoch: u32,
    pub issued: u32,
    pub remaining: u32,
    pub funding: U128,
    pub owner_key: PublicKey,
    pub expires_at_ns: U64,
}

#[derive(Serialize, Deserialize)]
#[serde(crate = "near_sdk::serde")]
pub struct OpenerStats {
    pub opened: u64,
    pub failed: u64,
}

#[ext_contract(ext_opener)]
pub trait OpenerCallbacks {
    fn on_name_opened(&mut self, grantee: AccountId, epoch: u32, name: AccountId);
}

fn grant_key(grantee: &AccountId) -> Vec<u8> {
    let mut key = GRANT_PREFIX.to_vec();
    key.extend_from_slice(grantee.as_bytes());
    key
}

fn approved() -> LookupMap<(AccountId, u32, AccountId), bool> {
    LookupMap::new(APPROVED_PREFIX.to_vec())
}

fn epoch_key(grantee: &AccountId) -> Vec<u8> {
    let mut key = EPOCH_PREFIX.to_vec();
    key.extend_from_slice(grantee.as_bytes());
    key
}

fn bump_epoch(grantee: &AccountId) -> u32 {
    let key = epoch_key(grantee);
    let next = env::storage_read(&key)
        .and_then(|b| <u32 as BorshDeserialize>::try_from_slice(&b).ok())
        .unwrap_or(0)
        .saturating_add(1);
    let bytes = next
        .try_to_vec()
        .unwrap_or_else(|_| env::panic_str("failed to serialize epoch"));
    env::storage_write(&key, &bytes);
    next
}

fn read_grant(grantee: &AccountId) -> Option<Grant> {
    env::storage_read(&grant_key(grantee)).and_then(|b| Grant::try_from_slice(&b).ok())
}

fn write_grant(grantee: &AccountId, grant: &Grant) {
    let bytes = grant
        .try_to_vec()
        .unwrap_or_else(|_| env::panic_str("failed to serialize grant"));
    env::storage_write(&grant_key(grantee), &bytes);
}

fn read_counter(key: &[u8]) -> u64 {
    env::storage_read(key)
        .and_then(|b| <u64 as BorshDeserialize>::try_from_slice(&b).ok())
        .unwrap_or(0)
}

fn bump_counter(key: &[u8]) {
    let next = read_counter(key).saturating_add(1);
    let bytes = next
        .try_to_vec()
        .unwrap_or_else(|_| env::panic_str("failed to serialize counter"));
    env::storage_write(key, &bytes);
}

fn assert_openable(names: &[AccountId]) {
    let mut seen: Vec<&str> = names.iter().map(|n| n.as_str()).collect();
    seen.sort_unstable();
    let before = seen.len();
    seen.dedup();
    crate::assert(seen.len() == before, error::DUPLICATE_NAME);
    for name in names {
        let raw = name.as_str();
        crate::assert(!raw.contains('.'), error::NOT_TOP_LEVEL);
        crate::assert(raw.len() >= MIN_TLA_LEN, error::NAME_TOO_SHORT);
        crate::assert(raw.len() <= MAX_TLA_LEN, error::NAME_TOO_LONG);
    }
}

#[near_bindgen]
impl MultiSigContract {
    pub fn grant_names(
        &mut self,
        grantee: AccountId,
        names: Vec<AccountId>,
        owner_key: PublicKey,
        funding: U128,
        expires_at_ns: U64,
    ) {
        self.assert_opener_self();
        let now = env::block_timestamp();
        crate::assert(expires_at_ns.0 > now, error::EXPIRY_IN_PAST);
        crate::assert(!names.is_empty(), error::EMPTY_BATCH);
        crate::assert(names.len() <= MAX_NAMES_PER_GRANT, error::GRANT_TOO_LARGE);
        crate::assert(funding.0 >= MIN_FUNDING, error::FUNDING_TOO_LOW);
        assert_openable(&names);
        if let Some(live) = read_grant(&grantee) {
            crate::assert(
                live.remaining == 0 || live.expires_at_ns.0 <= now,
                error::GRANT_LIVE,
            );
        }
        let epoch = bump_epoch(&grantee);
        let mut set = approved();
        for name in &names {
            set.insert(&(grantee.clone(), epoch, name.clone()), &true);
        }
        write_grant(
            &grantee,
            &Grant {
                epoch,
                issued: names.len() as u32,
                remaining: names.len() as u32,
                funding,
                owner_key,
                expires_at_ns,
            },
        );
    }

    pub fn revoke_grant(&mut self, grantee: AccountId) {
        self.assert_opener_self();
        bump_epoch(&grantee);
        env::storage_remove(&grant_key(&grantee));
    }

    pub fn open_names(&mut self, names: Vec<AccountId>) -> u32 {
        let caller = env::predecessor_account_id();
        let grant = self.charge_opener_grant(&caller, &names);
        let taken = names.len() as u32;
        let mut set = approved();
        for name in names {
            crate::assert(
                set.remove(&(caller.clone(), grant.epoch, name.clone()))
                    .is_some(),
                error::NOT_APPROVED,
            );
            Promise::new(name.clone())
                .create_account()
                .transfer(grant.funding.0)
                .add_full_access_key(grant.owner_key.clone())
                .then(ext_opener::on_name_opened(
                    caller.clone(),
                    grant.epoch,
                    name,
                    env::current_account_id(),
                    0,
                    GAS_FOR_CALLBACK,
                ));
        }
        taken
    }

    #[private]
    pub fn on_name_opened(&mut self, grantee: AccountId, epoch: u32, name: AccountId) {
        if is_promise_success() {
            bump_counter(OPENED_KEY);
            return;
        }
        bump_counter(FAILED_KEY);
        if let Some(mut grant) = read_grant(&grantee) {
            if grant.epoch == epoch && grant.remaining < grant.issued {
                grant.remaining += 1;
                write_grant(&grantee, &grant);
                approved().insert(&(grantee, epoch, name), &true);
            }
        }
    }

    pub fn get_grant(&self, grantee: AccountId) -> Option<Grant> {
        read_grant(&grantee)
    }

    pub fn is_name_approved(&self, grantee: AccountId, name: AccountId) -> bool {
        match read_grant(&grantee) {
            Some(grant) => approved()
                .get(&(grantee, grant.epoch, name))
                .unwrap_or(false),
            None => false,
        }
    }

    pub fn opener_stats(&self) -> OpenerStats {
        OpenerStats {
            opened: read_counter(OPENED_KEY),
            failed: read_counter(FAILED_KEY),
        }
    }

    fn assert_multisig_sound(&self) {
        let members = self.get_members().len();
        let threshold = self.get_num_confirmations();
        crate::assert(
            threshold > 0 && members >= threshold as usize,
            error::MULTISIG_UNSOUND,
        );
    }

    fn assert_opener_self(&self) {
        self.assert_multisig_sound();
        crate::assert(
            env::predecessor_account_id() == env::current_account_id(),
            error::ONLY_SELF,
        );
    }

    fn charge_opener_grant(&mut self, caller: &AccountId, names: &[AccountId]) -> Grant {
        self.assert_multisig_sound();
        let count = names.len();
        crate::assert(count > 0, error::EMPTY_BATCH);
        crate::assert(count <= MAX_NAMES_PER_CALL, error::BATCH_TOO_LARGE);
        assert_openable(names);
        crate::assert(
            env::prepaid_gas().0 >= GAS_PER_NAME.0.saturating_mul(count as u64),
            error::GAS_TOO_LOW,
        );
        let mut grant = read_grant(caller).unwrap_or_else(|| env::panic_str(error::NO_GRANT));
        crate::assert(
            grant.expires_at_ns.0 > env::block_timestamp(),
            error::GRANT_EXPIRED,
        );
        crate::assert(grant.remaining >= count as u32, error::GRANT_EXHAUSTED);
        let reserve = env::storage_byte_cost().saturating_mul(env::storage_usage() as Balance);
        let spendable = env::account_balance().saturating_sub(reserve);
        crate::assert(
            spendable > grant.funding.0.saturating_mul(count as Balance),
            error::BALANCE_TOO_LOW,
        );
        grant.remaining -= count as u32;
        write_grant(caller, &grant);
        grant
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use near_sdk::mock::VmAction;
    use near_sdk::serde_json;
    use near_sdk::test_utils::{
        get_created_receipts, testing_env_with_promise_results, VMContextBuilder,
    };
    use near_sdk::{testing_env, PromiseResult, VMContext};

    use super::*;
    use crate::{MultiSigContract, MultisigMember, StorageKeys};

    const TGAS: u64 = 1_000_000_000_000;
    const ONE_NEAR: Balance = 1_000_000_000_000_000_000_000_000;
    const NOW: u64 = 1_000;
    const EXPIRY: u64 = 1_000_000;
    const FUNDING: Balance = 20_000_000_000_000_000_000_000;
    const FULL_GAS: Gas = Gas(300_000_000_000_000);
    const MOCK_RUNNABLE_BATCH: usize = 12;
    const STORAGE_BYTES: u64 = 300_000;
    const LAUNCH_COHORT: usize = 528;

    fn assert_refused(test_path: &str, expected: &str) {
        let binary = std::env::current_exe().expect("the test binary must know its own path");
        let run = Command::new(binary)
            .args([
                "--exact",
                "--include-ignored",
                "--nocapture",
                "--test-threads=1",
                test_path,
            ])
            .output()
            .expect("failed to re-run the test binary");
        let stderr = String::from_utf8_lossy(&run.stderr);
        let stdout = String::from_utf8_lossy(&run.stdout);
        assert!(
            stdout.contains("running 1 test"),
            "{} did not name a test, so nothing was exercised:\n{}",
            test_path,
            stdout
        );
        assert!(
            !run.status.success(),
            "{} was expected to be refused but it succeeded",
            test_path
        );
        assert!(
            stderr.contains(expected),
            "{} did not refuse with {:?}, stderr was:\n{}",
            test_path,
            expected,
            stderr
        );
    }

    macro_rules! refuses {
        ($name:ident, $expected:expr, $body:block) => {
            mod $name {
                use super::*;

                #[test]
                #[ignore]
                fn subject() $body

                #[test]
                fn driver() {
                    assert_refused(
                        concat!("opener::tests::", stringify!($name), "::subject"),
                        $expected,
                    );
                }
            }
        };
    }

    fn id(raw: &str) -> AccountId {
        AccountId::new_unchecked(raw.to_string())
    }

    fn registrar() -> AccountId {
        id("registrar")
    }

    fn opener() -> AccountId {
        id("opener.near")
    }

    fn stranger() -> AccountId {
        id("mallory.near")
    }

    fn owner_key() -> PublicKey {
        "ed25519:Eg2jtsiMrprn7zgKKUk79qM1hWhANsFyE6JSX4txLEuy"
            .parse()
            .unwrap()
    }

    fn other_key() -> PublicKey {
        "ed25519:6E8sCci9badyRkXb3JoRpBj5p8C6Tw41ELDZoiihKEtp"
            .parse()
            .unwrap()
    }

    fn names(raw: &[&str]) -> Vec<AccountId> {
        raw.iter().map(|n| id(n)).collect()
    }

    fn generated(count: usize) -> Vec<AccountId> {
        (0..count).map(|i| id(&format!("name{:05}", i))).collect()
    }

    fn context(predecessor: AccountId, gas: Gas, balance: Balance, now: u64) -> VMContext {
        VMContextBuilder::new()
            .current_account_id(registrar())
            .predecessor_account_id(predecessor.clone())
            .signer_account_id(predecessor)
            .account_balance(balance)
            .prepaid_gas(gas)
            .block_timestamp(now)
            .build()
    }

    fn spendable_context(predecessor: AccountId, spendable: Balance) -> VMContext {
        let reserve = env::storage_byte_cost().saturating_mul(STORAGE_BYTES as Balance);
        VMContextBuilder::new()
            .current_account_id(registrar())
            .predecessor_account_id(predecessor.clone())
            .signer_account_id(predecessor)
            .account_balance(reserve.saturating_add(spendable))
            .storage_usage(STORAGE_BYTES)
            .prepaid_gas(FULL_GAS)
            .block_timestamp(NOW)
            .build()
    }

    fn as_council() -> VMContext {
        context(registrar(), FULL_GAS, ONE_NEAR * 1_000, NOW)
    }

    fn as_opener() -> VMContext {
        context(opener(), FULL_GAS, ONE_NEAR * 1_000, NOW)
    }

    fn council_of(members: usize, threshold: u32) -> MultiSigContract {
        testing_env!(as_council());
        let list: Vec<MultisigMember> = (0..members)
            .map(|i| MultisigMember::Account {
                account_id: id(&format!("council{}.near", i)),
            })
            .collect();
        MultiSigContract::new(list, threshold)
    }

    fn installed() -> MultiSigContract {
        council_of(4, 2)
    }

    fn grant_to(contract: &mut MultiSigContract, grantee: AccountId, list: Vec<AccountId>) {
        testing_env!(as_council());
        contract.grant_names(grantee, list, owner_key(), U128(FUNDING), U64(EXPIRY));
    }

    fn granted(list: &[&str]) -> MultiSigContract {
        let mut contract = installed();
        grant_to(&mut contract, opener(), names(list));
        contract
    }

    fn fail_the_callback(contract: &mut MultiSigContract, epoch: u32, name: &str) {
        testing_env_with_promise_results(as_council(), PromiseResult::Failed);
        contract.on_name_opened(opener(), epoch, id(name));
    }

    fn succeed_the_callback(contract: &mut MultiSigContract, epoch: u32, name: &str) {
        testing_env_with_promise_results(as_council(), PromiseResult::Successful(Vec::new()));
        contract.on_name_opened(opener(), epoch, id(name));
    }

    refuses!(a_stranger_cannot_grant_names, error::ONLY_SELF, {
        let mut contract = installed();
        testing_env!(context(stranger(), FULL_GAS, ONE_NEAR * 1_000, NOW));
        contract.grant_names(
            stranger(),
            names(&["alpha"]),
            owner_key(),
            U128(FUNDING),
            U64(EXPIRY),
        );
    });

    refuses!(a_council_member_cannot_grant_directly, error::ONLY_SELF, {
        let mut contract = installed();
        testing_env!(context(id("council0.near"), FULL_GAS, ONE_NEAR, NOW));
        contract.grant_names(
            opener(),
            names(&["alpha"]),
            owner_key(),
            U128(FUNDING),
            U64(EXPIRY),
        );
    });

    refuses!(a_council_member_cannot_revoke_directly, error::ONLY_SELF, {
        let mut contract = granted(&["alpha"]);
        testing_env!(context(id("council0.near"), FULL_GAS, ONE_NEAR, NOW));
        contract.revoke_grant(opener());
    });

    refuses!(a_grantee_cannot_extend_their_own_grant, error::ONLY_SELF, {
        let mut contract = granted(&["alpha"]);
        testing_env!(as_opener());
        contract.grant_names(
            opener(),
            names(&["bravo"]),
            owner_key(),
            U128(FUNDING),
            U64(EXPIRY),
        );
    });

    refuses!(
        a_zero_threshold_multisig_cannot_grant,
        error::MULTISIG_UNSOUND,
        {
            let mut contract = council_of(4, 0);
            grant_to(&mut contract, opener(), names(&["alpha"]));
        }
    );

    refuses!(
        a_threshold_above_the_member_count_cannot_grant,
        error::MULTISIG_UNSOUND,
        {
            let mut contract = installed();
            contract.num_confirmations = 9;
            grant_to(&mut contract, opener(), names(&["alpha"]));
        }
    );

    refuses!(
        a_threshold_above_the_member_count_cannot_open,
        error::MULTISIG_UNSOUND,
        {
            let mut contract = granted(&["alpha"]);
            contract.num_confirmations = 9;
            testing_env!(as_opener());
            contract.open_names(names(&["alpha"]));
        }
    );

    refuses!(
        a_threshold_above_the_member_count_cannot_revoke,
        error::MULTISIG_UNSOUND,
        {
            let mut contract = granted(&["alpha"]);
            contract.num_confirmations = 9;
            testing_env!(as_council());
            contract.revoke_grant(opener());
        }
    );

    refuses!(an_empty_grant_is_refused, error::EMPTY_BATCH, {
        let mut contract = installed();
        grant_to(&mut contract, opener(), Vec::new());
    });

    refuses!(
        a_grant_over_the_per_grant_limit_is_refused,
        error::GRANT_TOO_LARGE,
        {
            let mut contract = installed();
            grant_to(&mut contract, opener(), generated(MAX_NAMES_PER_GRANT + 1));
        }
    );

    refuses!(
        funding_one_yocto_below_the_floor_is_refused,
        error::FUNDING_TOO_LOW,
        {
            let mut contract = installed();
            testing_env!(as_council());
            contract.grant_names(
                opener(),
                names(&["alpha"]),
                owner_key(),
                U128(MIN_FUNDING - 1),
                U64(EXPIRY),
            );
        }
    );

    refuses!(an_expiry_in_the_past_is_refused, error::EXPIRY_IN_PAST, {
        let mut contract = installed();
        testing_env!(as_council());
        contract.grant_names(
            opener(),
            names(&["alpha"]),
            owner_key(),
            U128(FUNDING),
            U64(NOW - 1),
        );
    });

    refuses!(
        an_expiry_at_the_current_block_is_refused,
        error::EXPIRY_IN_PAST,
        {
            let mut contract = installed();
            testing_env!(as_council());
            contract.grant_names(
                opener(),
                names(&["alpha"]),
                owner_key(),
                U128(FUNDING),
                U64(NOW),
            );
        }
    );

    refuses!(
        a_duplicate_name_in_the_grant_is_refused,
        error::DUPLICATE_NAME,
        {
            let mut contract = installed();
            grant_to(&mut contract, opener(), names(&["alpha", "bravo", "alpha"]));
        }
    );

    refuses!(a_sub_account_cannot_be_granted, error::NOT_TOP_LEVEL, {
        let mut contract = installed();
        grant_to(&mut contract, opener(), names(&["alpha.near"]));
    });

    refuses!(a_two_character_name_is_refused, error::NAME_TOO_SHORT, {
        let mut contract = installed();
        grant_to(&mut contract, opener(), names(&["ai"]));
    });

    refuses!(
        a_second_grant_cannot_discard_an_unspent_one,
        error::GRANT_LIVE,
        {
            let mut contract = granted(&["alpha", "bravo"]);
            grant_to(&mut contract, opener(), names(&["charlie"]));
        }
    );

    refuses!(
        a_name_from_a_revoked_grant_cannot_be_opened,
        error::NOT_APPROVED,
        {
            let mut contract = granted(&["alpha"]);
            testing_env!(as_council());
            contract.revoke_grant(opener());
            grant_to(&mut contract, opener(), names(&["bravo"]));
            testing_env!(as_opener());
            contract.open_names(names(&["alpha"]));
        }
    );

    refuses!(a_caller_without_a_grant_opens_nothing, error::NO_GRANT, {
        let mut contract = granted(&["alpha"]);
        testing_env!(context(stranger(), FULL_GAS, ONE_NEAR * 1_000, NOW));
        contract.open_names(names(&["alpha"]));
    });

    refuses!(
        a_name_outside_the_grant_cannot_be_opened,
        error::NOT_APPROVED,
        {
            let mut contract = granted(&["alpha", "bravo"]);
            testing_env!(as_opener());
            contract.open_names(names(&["charlie"]));
        }
    );

    refuses!(a_name_cannot_be_opened_twice, error::NOT_APPROVED, {
        let mut contract = granted(&["alpha", "bravo"]);
        testing_env!(as_opener());
        contract.open_names(names(&["alpha"]));
        testing_env!(as_opener());
        contract.open_names(names(&["alpha"]));
    });

    refuses!(an_expired_grant_opens_nothing, error::GRANT_EXPIRED, {
        let mut contract = granted(&["alpha"]);
        testing_env!(context(opener(), FULL_GAS, ONE_NEAR * 1_000, EXPIRY + 1));
        contract.open_names(names(&["alpha"]));
    });

    refuses!(an_empty_open_batch_is_refused, error::EMPTY_BATCH, {
        let mut contract = granted(&["alpha"]);
        testing_env!(as_opener());
        contract.open_names(Vec::new());
    });

    refuses!(
        a_batch_one_over_the_per_call_limit_is_refused,
        error::BATCH_TOO_LARGE,
        {
            let mut contract = installed();
            grant_to(&mut contract, opener(), generated(MAX_NAMES_PER_CALL + 1));
            testing_env!(as_opener());
            contract.open_names(generated(MAX_NAMES_PER_CALL + 1));
        }
    );

    refuses!(
        a_repeated_name_in_one_open_batch_is_refused,
        error::DUPLICATE_NAME,
        {
            let mut contract = granted(&["alpha", "bravo"]);
            testing_env!(as_opener());
            contract.open_names(names(&["alpha", "alpha"]));
        }
    );

    refuses!(
        one_gas_unit_below_the_requirement_is_refused,
        error::GAS_TOO_LOW,
        {
            let mut contract = granted(&["alpha"]);
            testing_env!(context(
                opener(),
                Gas(GAS_PER_NAME.0 - 1),
                ONE_NEAR * 1_000,
                NOW
            ));
            contract.open_names(names(&["alpha"]));
        }
    );

    refuses!(
        gas_exactly_at_the_requirement_clears_the_gas_check,
        error::NO_GRANT,
        {
            let mut contract = installed();
            testing_env!(context(stranger(), GAS_PER_NAME, ONE_NEAR * 1_000, NOW));
            contract.open_names(names(&["alpha"]));
        }
    );

    refuses!(one_name_of_gas_will_not_open_three, error::GAS_TOO_LOW, {
        let mut contract = granted(&["alpha", "bravo", "charlie"]);
        testing_env!(context(opener(), GAS_PER_NAME, ONE_NEAR * 1_000, NOW));
        contract.open_names(names(&["alpha", "bravo", "charlie"]));
    });

    refuses!(
        a_grant_cannot_be_overspent_across_two_calls,
        error::GRANT_EXHAUSTED,
        {
            let mut contract = granted(&["alpha", "bravo", "charlie"]);
            testing_env!(as_opener());
            contract.open_names(names(&["alpha", "bravo"]));
            testing_env!(as_opener());
            contract.open_names(names(&["charlie", "alpha"]));
        }
    );

    refuses!(
        a_balance_below_the_funding_is_refused,
        error::BALANCE_TOO_LOW,
        {
            let mut contract = granted(&["alpha"]);
            testing_env!(spendable_context(opener(), FUNDING / 2));
            contract.open_names(names(&["alpha"]));
        }
    );

    refuses!(
        a_balance_exactly_equal_to_the_funding_is_refused,
        error::BALANCE_TOO_LOW,
        {
            let mut contract = granted(&["alpha"]);
            testing_env!(spendable_context(opener(), FUNDING));
            contract.open_names(names(&["alpha"]));
        }
    );

    refuses!(
        the_balance_check_scales_with_the_batch_size,
        error::BALANCE_TOO_LOW,
        {
            let mut contract = granted(&["alpha", "bravo"]);
            testing_env!(spendable_context(opener(), FUNDING * 2));
            contract.open_names(names(&["alpha", "bravo"]));
        }
    );

    #[test]
    fn a_multisig_at_exactly_its_threshold_is_sound() {
        let mut contract = council_of(2, 2);
        grant_to(&mut contract, opener(), names(&["alpha"]));
        assert_eq!(contract.get_grant(opener()).unwrap().remaining, 1);
    }

    #[test]
    fn a_grant_at_exactly_the_per_grant_limit_is_accepted() {
        let mut contract = installed();
        grant_to(&mut contract, opener(), generated(MAX_NAMES_PER_GRANT));
        let grant = contract.get_grant(opener()).unwrap();
        assert_eq!(grant.issued, MAX_NAMES_PER_GRANT as u32);
        assert_eq!(grant.remaining, MAX_NAMES_PER_GRANT as u32);
    }

    #[test]
    fn funding_exactly_at_the_floor_is_accepted() {
        let mut contract = installed();
        testing_env!(as_council());
        contract.grant_names(
            opener(),
            names(&["alpha"]),
            owner_key(),
            U128(MIN_FUNDING),
            U64(EXPIRY),
        );
        assert_eq!(contract.get_grant(opener()).unwrap().funding.0, MIN_FUNDING);
    }

    #[test]
    fn a_three_character_name_is_accepted() {
        let mut contract = installed();
        grant_to(&mut contract, opener(), names(&["aaa", "bbb", "ccc"]));
        assert_eq!(contract.get_grant(opener()).unwrap().issued, 3);
    }

    #[test]
    fn a_name_at_exactly_the_account_id_ceiling_is_accepted() {
        let mut contract = installed();
        let longest = id(&"a".repeat(MAX_TLA_LEN));
        grant_to(&mut contract, opener(), vec![longest]);
        assert_eq!(contract.get_grant(opener()).unwrap().issued, 1);
    }

    #[test]
    fn a_fully_spent_grant_can_be_replaced_without_a_revoke() {
        let mut contract = granted(&["alpha"]);
        testing_env!(as_opener());
        contract.open_names(names(&["alpha"]));
        grant_to(&mut contract, opener(), names(&["bravo", "charlie"]));
        assert_eq!(contract.get_grant(opener()).unwrap().remaining, 2);
    }

    #[test]
    fn an_expired_grant_can_be_replaced_without_a_revoke() {
        let mut contract = granted(&["alpha", "bravo"]);
        testing_env!(context(registrar(), FULL_GAS, ONE_NEAR * 1_000, EXPIRY + 1));
        contract.grant_names(
            opener(),
            names(&["charlie"]),
            owner_key(),
            U128(FUNDING),
            U64(EXPIRY * 2),
        );
        assert_eq!(contract.get_grant(opener()).unwrap().remaining, 1);
    }

    #[test]
    fn a_grant_records_the_count_the_funding_the_key_and_the_expiry() {
        let mut contract = installed();
        testing_env!(as_council());
        contract.grant_names(
            opener(),
            names(&["alpha", "bravo"]),
            other_key(),
            U128(FUNDING),
            U64(EXPIRY),
        );
        let grant = contract.get_grant(opener()).unwrap();
        assert_eq!(grant.issued, 2);
        assert_eq!(grant.remaining, 2);
        assert_eq!(grant.funding.0, FUNDING);
        assert_eq!(grant.owner_key, other_key());
        assert_eq!(grant.expires_at_ns.0, EXPIRY);
        assert_eq!(grant.epoch, 1);
    }

    #[test]
    fn only_the_granted_names_are_approved() {
        let contract = granted(&["alpha", "bravo"]);
        assert!(contract.is_name_approved(opener(), id("alpha")));
        assert!(contract.is_name_approved(opener(), id("bravo")));
        assert!(!contract.is_name_approved(opener(), id("charlie")));
    }

    #[test]
    fn an_account_with_no_grant_has_no_approved_names() {
        let contract = granted(&["alpha"]);
        assert!(!contract.is_name_approved(stranger(), id("alpha")));
        assert!(contract.get_grant(stranger()).is_none());
    }

    #[test]
    fn the_epoch_advances_on_every_grant() {
        let mut contract = granted(&["alpha"]);
        testing_env!(as_opener());
        contract.open_names(names(&["alpha"]));
        grant_to(&mut contract, opener(), names(&["bravo"]));
        assert_eq!(contract.get_grant(opener()).unwrap().epoch, 2);
    }

    #[test]
    fn the_epoch_survives_a_revoke() {
        let mut contract = granted(&["alpha"]);
        testing_env!(as_council());
        contract.revoke_grant(opener());
        grant_to(&mut contract, opener(), names(&["bravo"]));
        assert_eq!(
            contract.get_grant(opener()).unwrap().epoch,
            3,
            "revoke must advance the epoch, or revoked approvals come back to life"
        );
    }

    #[test]
    fn a_revoked_grant_leaves_no_approved_names_behind() {
        let mut contract = granted(&["alpha"]);
        testing_env!(as_council());
        contract.revoke_grant(opener());
        assert!(contract.get_grant(opener()).is_none());
        grant_to(&mut contract, opener(), names(&["bravo"]));
        assert!(!contract.is_name_approved(opener(), id("alpha")));
        assert!(contract.is_name_approved(opener(), id("bravo")));
    }

    #[test]
    fn two_grantees_hold_independent_epochs_and_grants() {
        let mut contract = installed();
        grant_to(&mut contract, opener(), names(&["alpha"]));
        grant_to(&mut contract, stranger(), names(&["bravo"]));
        assert_eq!(contract.get_grant(opener()).unwrap().epoch, 1);
        assert_eq!(contract.get_grant(stranger()).unwrap().epoch, 1);
        assert!(!contract.is_name_approved(opener(), id("bravo")));
        assert!(!contract.is_name_approved(stranger(), id("alpha")));
    }

    #[test]
    fn opening_names_spends_the_grant() {
        let mut contract = granted(&["alpha", "bravo", "charlie"]);
        testing_env!(as_opener());
        let taken = contract.open_names(names(&["alpha", "bravo"]));
        assert_eq!(taken, 2);
        assert_eq!(contract.get_grant(opener()).unwrap().remaining, 1);
        assert_eq!(contract.get_grant(opener()).unwrap().issued, 3);
    }

    #[test]
    fn a_grant_one_nanosecond_before_expiry_still_opens() {
        let mut contract = granted(&["alpha"]);
        testing_env!(context(opener(), FULL_GAS, ONE_NEAR * 1_000, EXPIRY - 1));
        assert_eq!(contract.open_names(names(&["alpha"])), 1);
    }

    #[test]
    fn a_multi_name_batch_spends_the_whole_grant_and_lands_every_name() {
        let mut contract = installed();
        grant_to(&mut contract, opener(), generated(MOCK_RUNNABLE_BATCH));
        testing_env!(as_opener());
        let taken = contract.open_names(generated(MOCK_RUNNABLE_BATCH));
        assert_eq!(taken, MOCK_RUNNABLE_BATCH as u32);
        assert_eq!(contract.get_grant(opener()).unwrap().remaining, 0);
        assert_eq!(get_created_receipts().len(), MOCK_RUNNABLE_BATCH * 2);
        for name in generated(MOCK_RUNNABLE_BATCH) {
            assert!(!contract.is_name_approved(opener(), name));
        }
    }

    #[test]
    fn the_name_ceiling_can_never_be_the_guard_that_fires() {
        let longest = "a".repeat(MAX_TLA_LEN);
        assert!(longest.parse::<AccountId>().is_ok());
        let over = "a".repeat(MAX_TLA_LEN + 1);
        assert!(
            over.parse::<AccountId>().is_err(),
            "AccountId already refuses anything over {} bytes, so MAX_TLA_LEN is depth not gate",
            MAX_TLA_LEN
        );
    }

    #[test]
    fn a_balance_one_yocto_above_the_funding_is_accepted() {
        let mut contract = granted(&["alpha"]);
        testing_env!(spendable_context(opener(), FUNDING + 1));
        assert_eq!(contract.open_names(names(&["alpha"])), 1);
    }

    #[test]
    fn each_opened_name_gets_a_create_transfer_key_receipt_and_a_callback() {
        let mut contract = granted(&["alpha", "bravo"]);
        testing_env!(as_opener());
        contract.open_names(names(&["alpha", "bravo"]));
        let receipts = get_created_receipts();
        assert_eq!(receipts.len(), 4);
        for (slot, expected) in ["alpha", "bravo"].iter().enumerate() {
            let creation = &receipts[slot * 2];
            assert_eq!(creation.receiver_id, id(expected));
            assert_eq!(creation.actions.len(), 3);
            assert!(matches!(creation.actions[0], VmAction::CreateAccount));
            match &creation.actions[1] {
                VmAction::Transfer { deposit } => assert_eq!(*deposit, FUNDING),
                other => panic!("expected a transfer, found {:?}", other),
            }
            match &creation.actions[2] {
                VmAction::AddKeyWithFullAccess { public_key, .. } => {
                    assert_eq!(*public_key, owner_key())
                }
                other => panic!("expected a full access key, found {:?}", other),
            }
            let callback = &receipts[slot * 2 + 1];
            assert_eq!(callback.receiver_id, registrar());
            assert_eq!(callback.receipt_indices, vec![(slot * 2) as u64]);
            assert_callback(&callback.actions[0], expected);
        }
    }

    fn assert_callback(action: &VmAction, expected: &str) {
        match action {
            VmAction::FunctionCall {
                method_name,
                args,
                gas,
                deposit,
            } => {
                assert_eq!(method_name, "on_name_opened");
                assert_eq!(*gas, GAS_FOR_CALLBACK);
                assert_eq!(*deposit, 0);
                let parsed: serde_json::Value = serde_json::from_slice(args).unwrap();
                assert_eq!(parsed["grantee"], "opener.near");
                assert_eq!(parsed["epoch"], 1);
                assert_eq!(parsed["name"], expected);
            }
            other => panic!("expected the opener callback, found {:?}", other),
        }
    }

    #[test]
    fn a_failed_creation_returns_the_slot_to_the_grant() {
        let mut contract = granted(&["alpha"]);
        testing_env!(as_opener());
        contract.open_names(names(&["alpha"]));
        assert_eq!(contract.get_grant(opener()).unwrap().remaining, 0);
        fail_the_callback(&mut contract, 1, "alpha");
        assert_eq!(contract.get_grant(opener()).unwrap().remaining, 1);
        assert!(contract.is_name_approved(opener(), id("alpha")));
        assert_eq!(contract.opener_stats().failed, 1);
        assert_eq!(contract.opener_stats().opened, 0);
    }

    #[test]
    fn a_returned_slot_can_be_spent_again() {
        let mut contract = granted(&["alpha"]);
        testing_env!(as_opener());
        contract.open_names(names(&["alpha"]));
        fail_the_callback(&mut contract, 1, "alpha");
        testing_env!(as_opener());
        assert_eq!(contract.open_names(names(&["alpha"])), 1);
    }

    #[test]
    fn a_successful_creation_counts_and_leaves_the_grant_alone() {
        let mut contract = granted(&["alpha", "bravo"]);
        testing_env!(as_opener());
        contract.open_names(names(&["alpha"]));
        succeed_the_callback(&mut contract, 1, "alpha");
        assert_eq!(contract.get_grant(opener()).unwrap().remaining, 1);
        assert!(!contract.is_name_approved(opener(), id("alpha")));
        assert_eq!(contract.opener_stats().opened, 1);
        assert_eq!(contract.opener_stats().failed, 0);
    }

    #[test]
    fn a_refund_cannot_push_a_grant_above_what_was_issued() {
        let mut contract = granted(&["alpha"]);
        testing_env!(as_opener());
        contract.open_names(names(&["alpha"]));
        for _ in 0..4 {
            fail_the_callback(&mut contract, 1, "alpha");
        }
        let grant = contract.get_grant(opener()).unwrap();
        assert_eq!(grant.issued, 1);
        assert_eq!(grant.remaining, 1);
        assert_eq!(contract.opener_stats().failed, 4);
    }

    #[test]
    fn a_callback_from_a_stale_epoch_refunds_nothing() {
        let mut contract = granted(&["alpha"]);
        testing_env!(as_opener());
        contract.open_names(names(&["alpha"]));
        testing_env!(as_council());
        contract.revoke_grant(opener());
        grant_to(&mut contract, opener(), names(&["bravo", "charlie"]));
        testing_env!(as_opener());
        contract.open_names(names(&["bravo"]));
        let before = contract.get_grant(opener()).unwrap();
        assert_eq!(before.epoch, 3);
        assert_eq!(before.issued, 2);
        assert_eq!(before.remaining, 1);
        fail_the_callback(&mut contract, 1, "alpha");
        let after = contract.get_grant(opener()).unwrap();
        assert_eq!(
            after.remaining, 1,
            "a failure from a revoked grant must not credit the current one"
        );
        assert!(!contract.is_name_approved(opener(), id("alpha")));
        assert!(contract.is_name_approved(opener(), id("charlie")));
    }

    #[test]
    fn a_callback_after_a_revoke_resurrects_nothing() {
        let mut contract = granted(&["alpha"]);
        testing_env!(as_opener());
        contract.open_names(names(&["alpha"]));
        testing_env!(as_council());
        contract.revoke_grant(opener());
        fail_the_callback(&mut contract, 1, "alpha");
        assert!(contract.get_grant(opener()).is_none());
        assert!(!contract.is_name_approved(opener(), id("alpha")));
        assert_eq!(contract.opener_stats().failed, 1);
    }

    #[test]
    fn the_stats_start_at_zero_and_count_both_outcomes() {
        let mut contract = granted(&["alpha", "bravo"]);
        assert_eq!(contract.opener_stats().opened, 0);
        assert_eq!(contract.opener_stats().failed, 0);
        testing_env!(as_opener());
        contract.open_names(names(&["alpha", "bravo"]));
        succeed_the_callback(&mut contract, 1, "alpha");
        fail_the_callback(&mut contract, 1, "bravo");
        assert_eq!(contract.opener_stats().opened, 1);
        assert_eq!(contract.opener_stats().failed, 1);
    }

    #[test]
    fn the_whole_launch_cohort_needs_this_many_calls() {
        let calls = LAUNCH_COHORT.div_ceil(MAX_NAMES_PER_CALL);
        assert_eq!(calls, 27);
        assert_eq!(
            calls * MAX_NAMES_PER_CALL * (GAS_PER_NAME.0 as usize) / (TGAS as usize),
            7_560
        );
    }

    #[test]
    fn no_opener_key_can_collide_with_the_multisig_state() {
        let upstream: Vec<Vec<u8>> = vec![
            b"STATE".to_vec(),
            StorageKeys::Members.try_to_vec().unwrap(),
            StorageKeys::Requests.try_to_vec().unwrap(),
            StorageKeys::Confirmations.try_to_vec().unwrap(),
            StorageKeys::NumRequestsPk.try_to_vec().unwrap(),
        ];
        for theirs in &upstream {
            for mine in &opener_keys() {
                assert!(
                    !theirs.starts_with(mine) && !mine.starts_with(theirs.as_slice()),
                    "opener key {:?} overlaps upstream key {:?}",
                    mine,
                    theirs
                );
            }
        }
    }

    #[test]
    fn no_two_opener_keys_shadow_each_other() {
        let ours = opener_keys();
        for (i, left) in ours.iter().enumerate() {
            for (j, right) in ours.iter().enumerate() {
                if i != j {
                    assert!(
                        !left.starts_with(right),
                        "opener key {:?} is a prefix of {:?}",
                        right,
                        left
                    );
                }
            }
        }
    }

    fn opener_keys() -> [&'static [u8]; 5] {
        [
            GRANT_PREFIX,
            EPOCH_PREFIX,
            APPROVED_PREFIX,
            OPENED_KEY,
            FAILED_KEY,
        ]
    }

    #[test]
    fn the_multisig_state_is_untouched_by_a_full_grant_and_open_cycle() {
        let mut contract = granted(&["alpha", "bravo"]);
        let members = contract.get_members();
        let nonce = contract.get_request_nonce();
        testing_env!(as_opener());
        contract.open_names(names(&["alpha", "bravo"]));
        succeed_the_callback(&mut contract, 1, "alpha");
        fail_the_callback(&mut contract, 1, "bravo");
        assert_eq!(contract.get_members(), members);
        assert_eq!(contract.get_num_confirmations(), 2);
        assert_eq!(contract.get_request_nonce(), nonce);
        assert!(contract.list_request_ids().is_empty());
    }

    #[test]
    fn upstream_refuses_a_request_deleted_before_the_cooldown() {
        assert_refused(
            "tests::test_panics_delete_request",
            "Request cannot be deleted immediately after creation.",
        );
    }

    #[test]
    fn upstream_refuses_a_request_deleted_by_the_wrong_key() {
        assert_refused(
            "tests::test_delete_request_panic_wrong_key",
            "Request cannot be deleted immediately after creation.",
        );
    }

    #[test]
    fn upstream_refuses_a_second_confirmation_from_one_key() {
        assert_refused(
            "tests::test_panics_on_second_confirm",
            "Already confirmed this request with this key",
        );
    }

    #[test]
    fn upstream_refuses_more_confirmations_than_members() {
        assert_refused(
            "tests::test_too_many_confirmations",
            "Members list must be equal or larger than number of confirmations",
        );
    }

    #[test]
    fn upstream_refuses_more_active_requests_than_the_limit() {
        assert_refused(
            "tests::test_too_many_requests",
            "Account has too many active requests. Confirm or delete some.",
        );
    }
}
