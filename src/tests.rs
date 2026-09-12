use near_sdk::test_utils::VMContextBuilder;
use near_sdk::{
    test_vm_config, testing_env, AccountId, Gas, NearToken, PromiseResult, PublicKey,
    RuntimeFeesConfig,
};
use sha2::{Digest, Sha256};

use super::*;

const NOTHING: NearToken = NearToken::from_yoctonear(0);
const YOCTO: NearToken = NearToken::from_yoctonear(1);
const FUNDING: NearToken = NearToken::from_millinear(20);
const NOW: u64 = 1_000_000;
const DELAY: u64 = MAINNET_UPGRADE_DELAY_NS;

fn here() -> AccountId {
    "registrar".parse().unwrap()
}

fn admin() -> AccountId {
    "council.sputnik-dao.near".parse().unwrap()
}

fn operator() -> AccountId {
    "operator.near".parse().unwrap()
}

fn next_operator() -> AccountId {
    "operator2.near".parse().unwrap()
}

fn stranger() -> AccountId {
    "stranger.near".parse().unwrap()
}

fn owner_key() -> PublicKey {
    "ed25519:6E8sCci9badyRkXb3JoRpBj5p8C6Tw41ELDZoiihKEtp"
        .parse()
        .unwrap()
}

fn other_key() -> PublicKey {
    "ed25519:HghiythFFPjVXwc9BLNi8uqFmfQc1DWFrJQ4nE6ANo7R"
        .parse()
        .unwrap()
}

fn context(predecessor: AccountId, deposit: NearToken) -> VMContextBuilder {
    let mut builder = VMContextBuilder::new();
    builder
        .current_account_id(here())
        .predecessor_account_id(predecessor)
        .attached_deposit(deposit)
        .prepaid_gas(Gas::from_tgas(300))
        .block_timestamp(NOW);
    builder
}

fn as_account(predecessor: AccountId, deposit: NearToken) {
    testing_env!(context(predecessor, deposit).build());
}

fn as_callback(result: PromiseResult) {
    testing_env!(
        context(here(), NOTHING).build(),
        test_vm_config(),
        RuntimeFeesConfig::test(),
        Default::default(),
        vec![result]
    );
}

fn installed() -> RegistrarOpener {
    as_account(here(), NOTHING);
    RegistrarOpener::new(admin(), operator(), U64(DELAY))
}

fn names(raw: &[&str]) -> Vec<AccountId> {
    raw.iter().map(|name| name.parse().unwrap()).collect()
}

fn expected_digest(owner_key: &PublicKey, funding: NearToken, added: &[&str]) -> CryptoHash {
    let mut seed = DIGEST_DOMAIN.to_vec();
    seed.extend_from_slice(owner_key.as_bytes());
    seed.extend_from_slice(&funding.as_yoctonear().to_le_bytes());
    let mut digest: CryptoHash = Sha256::digest(&seed).into();
    for name in added {
        let mut step = digest.to_vec();
        step.extend_from_slice(name.as_bytes());
        digest = Sha256::digest(&step).into();
    }
    digest
}

fn drafted(contract: &mut RegistrarOpener, raw: &[&str]) -> u32 {
    as_account(operator(), NOTHING);
    let batch_id = contract.create_batch(owner_key(), FUNDING);
    contract.add_names(batch_id, names(raw));
    batch_id
}

fn approved(contract: &mut RegistrarOpener, raw: &[&str]) -> u32 {
    let batch_id = drafted(contract, raw);
    let digest = contract.get_batch(batch_id).unwrap().digest;
    as_account(admin(), YOCTO);
    contract.approve_batch(batch_id, digest);
    batch_id
}

#[test]
fn install_sets_both_roles_and_starts_at_epoch_zero() {
    let contract = installed();
    let view = contract.opener_view();
    assert_eq!(view.admin, admin());
    assert_eq!(view.operator, operator());
    assert_eq!(view.operator_epoch, 0);
    assert_eq!(view.state_version, STATE_VERSION);
    assert!(view.pending_code.is_none());
    assert!(view.pending_admin.is_none());
}

#[test]
#[should_panic(expected = "only this account may call this")]
fn a_stranger_cannot_install_the_opener() {
    as_account(stranger(), NOTHING);
    RegistrarOpener::new(admin(), operator(), U64(DELAY));
}

#[test]
#[should_panic(expected = "admin and operator must be different accounts")]
fn the_two_roles_cannot_be_the_same_account() {
    as_account(here(), NOTHING);
    RegistrarOpener::new(admin(), admin(), U64(DELAY));
}

#[test]
#[should_panic(expected = "neither role may be this account")]
fn neither_role_may_be_the_registrar_itself() {
    as_account(here(), NOTHING);
    RegistrarOpener::new(here(), operator(), U64(DELAY));
}

#[test]
#[should_panic(expected = "this account already runs the opener")]
fn a_second_install_cannot_rename_the_admin() {
    let contract = installed();
    env::state_write(&contract);
    as_account(here(), NOTHING);
    RegistrarOpener::new(stranger(), operator(), U64(DELAY));
}

#[test]
#[should_panic(expected = "the upgrade delay must be greater than zero")]
fn an_instant_upgrade_path_cannot_be_configured_at_install() {
    as_account(here(), NOTHING);
    RegistrarOpener::new(admin(), operator(), U64(0));
}

#[test]
fn the_view_publishes_both_the_configured_delay_and_the_mainnet_one() {
    let contract = installed();
    let view = contract.opener_view();
    assert_eq!(view.upgrade_delay_ns.0, DELAY);
    assert_eq!(view.mainnet_upgrade_delay_ns.0, MAINNET_UPGRADE_DELAY_NS);
}

#[test]
fn a_new_batch_is_empty_unapproved_and_on_the_current_epoch() {
    let mut contract = installed();
    as_account(operator(), NOTHING);
    let batch_id = contract.create_batch(owner_key(), FUNDING);
    let batch = contract.get_batch(batch_id).unwrap();
    assert_eq!(batch.count, 0);
    assert_eq!(batch.remaining, 0);
    assert!(!batch.approved);
    assert!(!batch.stale);
    assert_eq!(batch.operator, operator());
    assert_eq!(batch.funding, FUNDING);
}

#[test]
fn the_digest_matches_an_independent_recomputation() {
    let mut contract = installed();
    let batch_id = drafted(&mut contract, &["aaa", "bbb", "ccc"]);
    let stored = CryptoHash::from(contract.get_batch(batch_id).unwrap().digest);
    assert_eq!(
        stored,
        expected_digest(&owner_key(), FUNDING, &["aaa", "bbb", "ccc"])
    );
}

#[test]
fn the_digest_is_the_same_whether_the_names_arrive_in_one_call_or_several() {
    let mut contract = installed();
    let one = drafted(&mut contract, &["aaa", "bbb", "ccc"]);
    as_account(operator(), NOTHING);
    let many = contract.create_batch(owner_key(), FUNDING);
    contract.add_names(many, names(&["aaa"]));
    contract.add_names(many, names(&["bbb", "ccc"]));
    assert_eq!(
        contract.get_batch(one).unwrap().digest,
        contract.get_batch(many).unwrap().digest
    );
}

#[test]
fn every_added_name_moves_the_digest() {
    let mut contract = installed();
    as_account(operator(), NOTHING);
    let batch_id = contract.create_batch(owner_key(), FUNDING);
    let seeded = contract.get_batch(batch_id).unwrap().digest;
    let after_one = contract.add_names(batch_id, names(&["aaa"]));
    let after_two = contract.add_names(batch_id, names(&["bbb"]));
    assert_ne!(seeded, after_one);
    assert_ne!(after_one, after_two);
}

#[test]
fn the_owner_key_and_the_funding_are_both_bound_into_the_digest() {
    let mut contract = installed();
    let baseline = drafted(&mut contract, &["aaa"]);
    as_account(operator(), NOTHING);
    let other_owner = contract.create_batch(other_key(), FUNDING);
    contract.add_names(other_owner, names(&["aaa"]));
    let other_funding = contract.create_batch(owner_key(), NearToken::from_millinear(30));
    contract.add_names(other_funding, names(&["aaa"]));
    let digest = contract.get_batch(baseline).unwrap().digest;
    assert_ne!(digest, contract.get_batch(other_owner).unwrap().digest);
    assert_ne!(digest, contract.get_batch(other_funding).unwrap().digest);
}

#[test]
#[should_panic(expected = "duplicate name in list")]
fn the_same_name_cannot_be_added_twice_across_calls() {
    let mut contract = installed();
    let batch_id = drafted(&mut contract, &["aaa"]);
    as_account(operator(), NOTHING);
    contract.add_names(batch_id, names(&["aaa"]));
}

#[test]
#[should_panic(expected = "duplicate name in list")]
fn the_same_name_cannot_be_added_twice_in_one_call() {
    let mut contract = installed();
    drafted(&mut contract, &["aaa", "aaa"]);
}

#[test]
#[should_panic(expected = "name is not a top level account")]
fn a_sub_account_cannot_be_added_to_a_batch() {
    let mut contract = installed();
    drafted(&mut contract, &["aaa.near"]);
}

#[test]
#[should_panic(expected = "short enough to be forgeable")]
fn a_two_character_name_is_refused() {
    let mut contract = installed();
    drafted(&mut contract, &["aa"]);
}

#[test]
#[should_panic(expected = "only the current operator may call this")]
fn a_stranger_cannot_draft_a_batch() {
    let mut contract = installed();
    as_account(stranger(), NOTHING);
    contract.create_batch(owner_key(), FUNDING);
}

#[test]
#[should_panic(expected = "only the current operator may call this")]
fn the_admin_cannot_draft_a_batch() {
    let mut contract = installed();
    as_account(admin(), NOTHING);
    contract.create_batch(owner_key(), FUNDING);
}

#[test]
fn approval_flips_the_batch_and_leaves_the_names_in_place() {
    let mut contract = installed();
    let batch_id = approved(&mut contract, &["aaa", "bbb"]);
    let batch = contract.get_batch(batch_id).unwrap();
    assert!(batch.approved);
    assert_eq!(batch.count, 2);
    assert_eq!(batch.remaining, 2);
}

#[test]
#[should_panic(expected = "digest does not match the batch contents")]
fn approving_a_digest_that_is_not_the_batch_is_refused() {
    let mut contract = installed();
    let batch_id = drafted(&mut contract, &["aaa", "bbb"]);
    let wrong = Base58CryptoHash::from(expected_digest(&owner_key(), FUNDING, &["aaa"]));
    as_account(admin(), YOCTO);
    contract.approve_batch(batch_id, wrong);
}

#[test]
#[should_panic(expected = "the batch holds no names")]
fn an_empty_batch_cannot_be_approved() {
    let mut contract = installed();
    as_account(operator(), NOTHING);
    let batch_id = contract.create_batch(owner_key(), FUNDING);
    let digest = contract.get_batch(batch_id).unwrap().digest;
    as_account(admin(), YOCTO);
    contract.approve_batch(batch_id, digest);
}

#[test]
#[should_panic(expected = "only the admin may call this")]
fn the_operator_cannot_approve_their_own_batch() {
    let mut contract = installed();
    let batch_id = drafted(&mut contract, &["aaa"]);
    let digest = contract.get_batch(batch_id).unwrap().digest;
    as_account(operator(), YOCTO);
    contract.approve_batch(batch_id, digest);
}

#[test]
#[should_panic(expected = "exactly one yoctoNEAR must be attached")]
fn approval_demands_one_yocto() {
    let mut contract = installed();
    let batch_id = drafted(&mut contract, &["aaa"]);
    let digest = contract.get_batch(batch_id).unwrap().digest;
    as_account(admin(), NOTHING);
    contract.approve_batch(batch_id, digest);
}

#[test]
#[should_panic(expected = "can no longer be edited")]
fn names_cannot_be_added_after_approval() {
    let mut contract = installed();
    let batch_id = approved(&mut contract, &["aaa"]);
    as_account(operator(), NOTHING);
    contract.add_names(batch_id, names(&["bbb"]));
}

#[test]
#[should_panic(expected = "can no longer be edited")]
fn a_batch_cannot_be_approved_twice() {
    let mut contract = installed();
    let batch_id = approved(&mut contract, &["aaa"]);
    let digest = contract.get_batch(batch_id).unwrap().digest;
    as_account(admin(), YOCTO);
    contract.approve_batch(batch_id, digest);
}

#[test]
fn replacing_the_operator_bumps_the_epoch_and_strands_an_approved_batch() {
    let mut contract = installed();
    let batch_id = approved(&mut contract, &["aaa"]);
    as_account(admin(), YOCTO);
    contract.change_operator(next_operator());
    let view = contract.opener_view();
    assert_eq!(view.operator, next_operator());
    assert_eq!(view.operator_epoch, 1);
    let batch = contract.get_batch(batch_id).unwrap();
    assert!(batch.approved);
    assert!(batch.stale);
}

#[test]
#[should_panic(expected = "the batch belongs to a replaced operator")]
fn a_stranded_batch_cannot_be_opened_by_the_new_operator() {
    let mut contract = installed();
    let batch_id = approved(&mut contract, &["aaa"]);
    as_account(admin(), YOCTO);
    contract.change_operator(next_operator());
    as_account(next_operator(), FUNDING);
    contract.open_names(batch_id, names(&["aaa"]));
}

#[test]
#[should_panic(expected = "only the current operator may call this")]
fn the_replaced_operator_loses_every_operator_method() {
    let mut contract = installed();
    as_account(admin(), YOCTO);
    contract.change_operator(next_operator());
    as_account(operator(), NOTHING);
    contract.create_batch(owner_key(), FUNDING);
}

#[test]
fn a_stranded_batch_can_be_discarded_to_reclaim_its_storage() {
    let mut contract = installed();
    let batch_id = approved(&mut contract, &["aaa"]);
    as_account(admin(), YOCTO);
    contract.change_operator(next_operator());
    as_account(next_operator(), NOTHING);
    contract.discard_batch(batch_id);
    assert!(contract.get_batch(batch_id).is_none());
}

#[test]
#[should_panic(expected = "an approved batch on the current operator cannot be discarded")]
fn a_live_approved_batch_cannot_be_discarded() {
    let mut contract = installed();
    let batch_id = approved(&mut contract, &["aaa"]);
    as_account(operator(), NOTHING);
    contract.discard_batch(batch_id);
}

#[test]
fn the_nominated_admin_governs_nothing_until_they_accept() {
    let mut contract = installed();
    as_account(admin(), YOCTO);
    contract.change_admin(stranger());
    assert_eq!(contract.opener_view().admin, admin());
    assert_eq!(contract.opener_view().pending_admin, Some(stranger()));
}

#[test]
#[should_panic(expected = "only the nominated admin may accept")]
fn only_the_nominee_can_accept_the_admin_role() {
    let mut contract = installed();
    as_account(admin(), YOCTO);
    contract.change_admin(next_operator());
    as_account(stranger(), YOCTO);
    contract.accept_admin();
}

#[test]
#[should_panic(expected = "no admin has been nominated")]
fn accepting_without_a_nomination_is_refused() {
    let mut contract = installed();
    as_account(stranger(), YOCTO);
    contract.accept_admin();
}

#[test]
fn accepting_moves_the_admin_and_clears_the_nomination() {
    let mut contract = installed();
    as_account(admin(), YOCTO);
    contract.change_admin(next_operator());
    as_account(next_operator(), YOCTO);
    contract.accept_admin();
    let view = contract.opener_view();
    assert_eq!(view.admin, next_operator());
    assert!(view.pending_admin.is_none());
}

#[test]
#[should_panic(expected = "the batch has not been approved")]
fn an_unapproved_batch_cannot_be_opened() {
    let mut contract = installed();
    let batch_id = drafted(&mut contract, &["aaa"]);
    as_account(operator(), FUNDING);
    contract.open_names(batch_id, names(&["aaa"]));
}

#[test]
#[should_panic(expected = "name is not in this batch")]
fn a_name_outside_the_approved_batch_cannot_be_opened() {
    let mut contract = installed();
    let batch_id = approved(&mut contract, &["aaa"]);
    as_account(operator(), FUNDING);
    contract.open_names(batch_id, names(&["zzz"]));
}

#[test]
#[should_panic(expected = "attached deposit must be the funding times the name count")]
fn opening_with_too_little_attached_is_refused() {
    let mut contract = installed();
    let batch_id = approved(&mut contract, &["aaa", "bbb"]);
    as_account(operator(), FUNDING);
    contract.open_names(batch_id, names(&["aaa", "bbb"]));
}

#[test]
#[should_panic(expected = "attached deposit must be the funding times the name count")]
fn opening_with_too_much_attached_is_refused() {
    let mut contract = installed();
    let batch_id = approved(&mut contract, &["aaa"]);
    as_account(operator(), NearToken::from_millinear(40));
    contract.open_names(batch_id, names(&["aaa"]));
}

#[test]
fn opening_takes_the_names_out_of_the_batch() {
    let mut contract = installed();
    let batch_id = approved(&mut contract, &["aaa", "bbb"]);
    as_account(operator(), FUNDING);
    assert_eq!(contract.open_names(batch_id, names(&["aaa"])), 1);
    let batch = contract.get_batch(batch_id).unwrap();
    assert_eq!(batch.count, 2);
    assert_eq!(batch.remaining, 1);
    assert_eq!(
        contract.list_batch_names(batch_id, None, None),
        names(&["bbb"])
    );
}

#[test]
#[should_panic(expected = "only the current operator may call this")]
fn a_stranger_cannot_open_an_approved_batch() {
    let mut contract = installed();
    let batch_id = approved(&mut contract, &["aaa"]);
    as_account(stranger(), FUNDING);
    contract.open_names(batch_id, names(&["aaa"]));
}

#[test]
#[should_panic(expected = "attach more gas or send fewer names")]
fn a_call_without_the_gas_for_its_names_is_refused() {
    let mut contract = installed();
    let batch_id = approved(&mut contract, &["aaa", "bbb", "ccc"]);
    testing_env!(context(operator(), NearToken::from_millinear(60))
        .prepaid_gas(Gas::from_tgas(20))
        .build());
    contract.open_names(batch_id, names(&["aaa", "bbb", "ccc"]));
}

#[test]
fn a_failed_open_puts_the_name_back_in_the_batch() {
    let mut contract = installed();
    let batch_id = approved(&mut contract, &["aaa"]);
    as_account(operator(), FUNDING);
    contract.open_names(batch_id, names(&["aaa"]));
    assert_eq!(contract.get_batch(batch_id).unwrap().remaining, 0);
    as_callback(PromiseResult::Failed);
    let handled = contract.on_name_opened(Some(batch_id), 0, Some("aaa".parse().unwrap()));
    assert!(!handled);
    let batch = contract.get_batch(batch_id).unwrap();
    assert_eq!(batch.remaining, 1);
    assert_eq!(contract.opener_view().failed, 1);
}

#[test]
fn a_failure_reported_against_a_replaced_operator_does_not_reopen_the_slot() {
    let mut contract = installed();
    let batch_id = approved(&mut contract, &["aaa"]);
    as_account(operator(), FUNDING);
    contract.open_names(batch_id, names(&["aaa"]));
    as_account(admin(), YOCTO);
    contract.change_operator(next_operator());
    as_callback(PromiseResult::Failed);
    contract.on_name_opened(Some(batch_id), 0, Some("aaa".parse().unwrap()));
    assert_eq!(contract.get_batch(batch_id).unwrap().remaining, 0);
}

#[test]
fn a_successful_open_is_counted() {
    let mut contract = installed();
    let batch_id = approved(&mut contract, &["aaa"]);
    as_account(operator(), FUNDING);
    contract.open_names(batch_id, names(&["aaa"]));
    as_callback(PromiseResult::Successful(Vec::new()));
    assert!(contract.on_name_opened(Some(batch_id), 0, Some("aaa".parse().unwrap())));
    assert_eq!(contract.opener_view().opened, 1);
    assert_eq!(contract.get_batch(batch_id).unwrap().remaining, 0);
}

#[test]
#[should_panic(expected = "only the admin may call this")]
fn the_operator_cannot_use_the_single_create_path() {
    let mut contract = installed();
    as_account(operator(), FUNDING);
    contract
        .create_account("aaa".parse().unwrap(), owner_key())
        .detach();
}

#[test]
#[should_panic(expected = "funding is below the account storage floor")]
fn the_single_create_path_refuses_dust() {
    let mut contract = installed();
    as_account(admin(), NearToken::from_yoctonear(1));
    contract
        .create_account("aaa".parse().unwrap(), owner_key())
        .detach();
}

#[test]
fn approving_code_records_the_hash_and_the_earliest_moment_it_can_land() {
    let mut contract = installed();
    as_account(admin(), YOCTO);
    contract.approve_code(Base58CryptoHash::from(CryptoHash::default()));
    let pending = contract.opener_view().pending_code.unwrap();
    assert_eq!(pending.earliest_at_ns.0, NOW + DELAY);
}

#[test]
#[should_panic(expected = "the approval delay has not elapsed")]
fn an_upgrade_inside_the_delay_is_refused() {
    let mut contract = installed();
    let code = vec![0u8, 1, 2, 3];
    let hash = Base58CryptoHash::from(to_hash(env::sha256(&code)));
    as_account(admin(), YOCTO);
    contract.approve_code(hash);
    as_account(admin(), NOTHING);
    contract.upgrade(code.into()).detach();
}

#[test]
#[should_panic(expected = "this code does not match the approved hash")]
fn code_that_does_not_match_the_approved_hash_is_refused() {
    let mut contract = installed();
    let approved_code = vec![0u8, 1, 2, 3];
    let hash = Base58CryptoHash::from(to_hash(env::sha256(&approved_code)));
    as_account(admin(), YOCTO);
    contract.approve_code(hash);
    testing_env!(context(admin(), NOTHING)
        .block_timestamp(NOW + DELAY)
        .build());
    contract.upgrade(vec![9u8, 9, 9].into()).detach();
}

#[test]
#[should_panic(expected = "no code hash has been approved")]
fn a_canceled_approval_cannot_be_upgraded_against() {
    let mut contract = installed();
    let code = vec![0u8, 1, 2, 3];
    let hash = Base58CryptoHash::from(to_hash(env::sha256(&code)));
    as_account(admin(), YOCTO);
    contract.approve_code(hash);
    as_account(admin(), YOCTO);
    contract.cancel_code();
    testing_env!(context(admin(), NOTHING)
        .block_timestamp(NOW + DELAY)
        .build());
    contract.upgrade(code.into()).detach();
}

#[test]
#[should_panic(expected = "only the admin or the operator may call this")]
fn a_stranger_cannot_execute_an_approved_upgrade() {
    let mut contract = installed();
    let code = vec![0u8, 1, 2, 3];
    let hash = Base58CryptoHash::from(to_hash(env::sha256(&code)));
    as_account(admin(), YOCTO);
    contract.approve_code(hash);
    testing_env!(context(stranger(), NOTHING)
        .block_timestamp(NOW + DELAY)
        .build());
    contract.upgrade(code.into()).detach();
}

#[test]
#[should_panic(expected = "more names than this call accepts")]
fn a_call_cannot_carry_more_names_than_the_documented_maximum() {
    let mut contract = installed();
    let raw: Vec<String> = (0..MAX_NAMES_PER_ADD + 1)
        .map(|index| format!("name{index:04}"))
        .collect();
    let batch_names: Vec<AccountId> = raw.iter().map(|name| name.parse().unwrap()).collect();
    as_account(operator(), NOTHING);
    let batch_id = contract.create_batch(owner_key(), FUNDING);
    contract.add_names(batch_id, batch_names);
}

#[test]
#[should_panic(expected = "the batch is at the per batch name limit")]
fn a_batch_cannot_grow_past_the_cohort_ceiling() {
    let mut contract = installed();
    as_account(operator(), NOTHING);
    let batch_id = contract.create_batch(owner_key(), FUNDING);
    let mut added = 0u32;
    while added <= MAX_NAMES_PER_BATCH {
        let chunk: Vec<AccountId> = (0..MAX_NAMES_PER_ADD)
            .map(|index| {
                format!("name{:06}", added as usize + index)
                    .parse()
                    .unwrap()
            })
            .collect();
        contract.add_names(batch_id, chunk);
        added += MAX_NAMES_PER_ADD as u32;
    }
}
