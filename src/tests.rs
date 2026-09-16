use near_sdk::json_types::Base58CryptoHash;
use near_sdk::test_utils::VMContextBuilder;
use near_sdk::{
    test_vm_config, testing_env, AccountId, CryptoHash, Gas, NearToken, PromiseResult, PublicKey,
    RuntimeFeesConfig,
};
use sha2::{Digest, Sha256};

use super::*;
use crate::batch::{MAX_LIVE_BATCHES, MAX_NAMES_PER_ADD};

const NOTHING: NearToken = NearToken::from_yoctonear(0);
const YOCTO: NearToken = NearToken::from_yoctonear(1);
const FUNDING: NearToken = NearToken::from_millinear(20);
const NOW: u64 = 1_000_000;
const DIGEST_DOMAIN: &[u8] = b"registrar-opener:batch:v1";

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
    RegistrarOpener::new(admin(), operator())
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
fn install_sets_both_roles_and_no_batches() {
    let contract = installed();
    let view = contract.opener_view();
    assert_eq!(view.admin, admin());
    assert_eq!(view.operator, operator());
    assert_eq!(view.live_batches, 0);
    assert_eq!(view.state_version, STATE_VERSION);
    assert_eq!(view.next_batch_id, 0);
    assert!(view.pending_admin.is_none());
}

#[test]
#[should_panic(expected = "only this account may call this")]
fn a_stranger_cannot_install_the_opener() {
    as_account(stranger(), NOTHING);
    RegistrarOpener::new(admin(), operator());
}

#[test]
#[should_panic(expected = "admin and operator must be different accounts")]
fn the_two_roles_cannot_be_the_same_account() {
    as_account(here(), NOTHING);
    RegistrarOpener::new(admin(), admin());
}

#[test]
#[should_panic(expected = "neither role may be this account")]
fn neither_role_may_be_the_registrar_itself() {
    as_account(here(), NOTHING);
    RegistrarOpener::new(here(), operator());
}

#[test]
#[should_panic(expected = "this account already runs the opener")]
fn a_second_install_cannot_rename_the_admin() {
    let contract = installed();
    env::state_write(&contract);
    as_account(here(), NOTHING);
    RegistrarOpener::new(stranger(), operator());
}

#[test]
fn a_new_batch_starts_empty_and_unapproved() {
    let mut contract = installed();
    as_account(operator(), NOTHING);
    let batch_id = contract.create_batch(owner_key(), FUNDING);
    let batch = contract.get_batch(batch_id).unwrap();
    assert_eq!(batch.count, 0);
    assert_eq!(batch.remaining, 0);
    assert!(!batch.approved);
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
#[should_panic(expected = "somebody's derived address")]
fn an_implicit_account_id_cannot_be_added_to_a_batch() {
    let mut contract = installed();
    drafted(
        &mut contract,
        &["98793cd91a3f870fb126f66285808c7e094afcfc4eda8a970f6648cdf0dbd6de"],
    );
}

#[test]
#[should_panic(expected = "discard a batch before drafting another")]
fn the_operator_cannot_hoard_batches_against_the_accounts_storage() {
    let mut contract = installed();
    as_account(operator(), NOTHING);
    for _ in 0..=MAX_LIVE_BATCHES {
        contract.create_batch(owner_key(), FUNDING);
    }
}

#[test]
fn discarding_frees_a_slot_for_the_next_batch() {
    let mut contract = installed();
    as_account(operator(), NOTHING);
    let mut ids = Vec::new();
    for _ in 0..MAX_LIVE_BATCHES {
        ids.push(contract.create_batch(owner_key(), FUNDING));
    }
    contract.discard_batch(ids[0]);
    let reopened = contract.create_batch(owner_key(), FUNDING);
    assert!(contract.get_batch(reopened).is_some());
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
fn replacing_the_operator_locks_the_old_one_out_and_leaves_the_batch_openable() {
    let mut contract = installed();
    let batch_id = approved(&mut contract, &["aaa"]);
    as_account(admin(), YOCTO);
    contract.change_operator(next_operator());
    assert_eq!(contract.opener_view().operator, next_operator());

    as_account(next_operator(), FUNDING);
    assert_eq!(contract.open_names(batch_id, names(&["aaa"])), 1);
    assert_eq!(contract.get_batch(batch_id).unwrap().remaining, 0);
}

#[test]
fn the_new_operator_can_discard_what_the_old_one_left_behind() {
    let mut contract = installed();
    let batch_id = drafted(&mut contract, &["aaa"]);
    as_account(admin(), YOCTO);
    contract.change_operator(next_operator());
    as_account(next_operator(), NOTHING);
    contract.discard_batch(batch_id);
    assert!(contract.get_batch(batch_id).is_none());
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
#[should_panic(expected = "an approved batch still holding names cannot be discarded")]
fn a_live_approved_batch_cannot_be_discarded() {
    let mut contract = installed();
    let batch_id = approved(&mut contract, &["aaa"]);
    as_account(operator(), NOTHING);
    contract.discard_batch(batch_id);
}

#[test]
fn the_admin_can_revoke_an_approved_batch_that_can_never_drain() {
    let mut contract = installed();
    let stuck = approved(&mut contract, &["aaa"]);
    as_account(operator(), NOTHING);
    for index in 1..MAX_LIVE_BATCHES {
        drafted(&mut contract, &[format!("bb{index}").as_str()]);
    }
    as_account(admin(), NOTHING);
    contract.discard_batch(stuck);
    assert!(contract.get_batch(stuck).is_none());

    as_account(operator(), NOTHING);
    let next = contract.create_batch(owner_key(), FUNDING);
    assert!(
        contract.get_batch(next).is_some(),
        "revoking an undrainable batch did not free its slot"
    );
}

#[test]
#[should_panic(expected = "an approved batch still holding names cannot be discarded")]
fn the_operator_cannot_discard_a_batch_the_council_approved() {
    let mut contract = installed();
    let batch_id = approved(&mut contract, &["aaa", "bbb"]);
    as_account(operator(), FUNDING);
    contract.open_names(batch_id, names(&["aaa"]));
    as_account(operator(), NOTHING);
    contract.discard_batch(batch_id);
}

#[test]
fn a_discarded_batch_id_is_never_reissued_to_a_later_batch() {
    let mut contract = installed();
    let first = drafted(&mut contract, &["aaa"]);
    as_account(operator(), NOTHING);
    contract.discard_batch(first);
    let second = drafted(&mut contract, &["bbb"]);
    assert_ne!(
        first, second,
        "a reissued id would inherit every name the dead batch stored"
    );
    assert!(
        !contract.is_in_batch(second, "aaa".parse().unwrap()),
        "the new batch can open a name the council never approved for it"
    );
}

#[test]
fn forgetting_frees_the_names_a_discarded_batch_stranded() {
    let mut contract = installed();
    let batch_id = drafted(&mut contract, &["aaa", "bbb"]);
    as_account(operator(), NOTHING);
    contract.discard_batch(batch_id);
    assert_eq!(contract.forget_names(batch_id, names(&["aaa", "bbb"])), 2);
    assert_eq!(
        contract.forget_names(batch_id, names(&["aaa", "bbb"])),
        0,
        "forgetting reported work it did not do the second time"
    );
}

#[test]
#[should_panic(expected = "discard the batch before forgetting what it held")]
fn a_live_batch_cannot_have_its_names_forgotten() {
    let mut contract = installed();
    let batch_id = drafted(&mut contract, &["aaa"]);
    as_account(operator(), NOTHING);
    contract.forget_names(batch_id, names(&["aaa"]));
}

#[test]
#[should_panic(expected = "no batch with that id")]
fn a_batch_id_that_was_never_issued_cannot_be_forgotten() {
    let mut contract = installed();
    as_account(operator(), NOTHING);
    contract.forget_names(7, names(&["aaa"]));
}

#[test]
#[should_panic(expected = "only the admin or the operator may call this")]
fn a_stranger_cannot_forget_names() {
    let mut contract = installed();
    let batch_id = drafted(&mut contract, &["aaa"]);
    as_account(operator(), NOTHING);
    contract.discard_batch(batch_id);
    as_account(stranger(), NOTHING);
    contract.forget_names(batch_id, names(&["aaa"]));
}

#[test]
fn forgetting_cannot_reach_a_name_a_live_batch_still_holds() {
    let mut contract = installed();
    let dead = drafted(&mut contract, &["aaa"]);
    let live = drafted(&mut contract, &["bbb"]);
    as_account(operator(), NOTHING);
    contract.discard_batch(dead);
    assert_eq!(contract.forget_names(dead, names(&["bbb"])), 0);
    assert!(
        contract.is_in_batch(live, "bbb".parse().unwrap()),
        "forgetting one batch reached into another"
    );
}

#[test]
fn a_fully_opened_batch_can_be_discarded_so_its_slot_comes_back() {
    let mut contract = installed();
    let mut ids = Vec::new();
    for index in 0..MAX_LIVE_BATCHES {
        let name = format!("aa{index}");
        let batch_id = approved(&mut contract, &[name.as_str()]);
        as_account(operator(), FUNDING);
        contract.open_names(batch_id, names(&[name.as_str()]));
        ids.push(batch_id);
    }
    as_account(operator(), NOTHING);
    contract.discard_batch(ids[0]);
    let next = contract.create_batch(owner_key(), FUNDING);
    assert!(
        contract.get_batch(next).is_some(),
        "a spent batch did not free its slot, the operator is bricked at the cap"
    );
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
    assert!(!contract.is_in_batch(batch_id, "aaa".parse().unwrap()));
    assert!(contract.is_in_batch(batch_id, "bbb".parse().unwrap()));
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
    let handled = contract.on_name_opened(batch_id, "aaa".parse().unwrap());
    assert!(!handled);
    let batch = contract.get_batch(batch_id).unwrap();
    assert_eq!(batch.remaining, 1);
    assert_eq!(contract.opener_view().failed, 1);
}

#[test]
fn a_failure_after_the_operator_changed_still_returns_the_slot() {
    let mut contract = installed();
    let batch_id = approved(&mut contract, &["aaa"]);
    as_account(operator(), FUNDING);
    contract.open_names(batch_id, names(&["aaa"]));
    as_account(admin(), YOCTO);
    contract.change_operator(next_operator());
    as_callback(PromiseResult::Failed);
    contract.on_name_opened(batch_id, "aaa".parse().unwrap());
    assert_eq!(
        contract.get_batch(batch_id).unwrap().remaining,
        1,
        "the council approved this name, a new operator should still be able to open it"
    );
}

#[test]
fn a_successful_open_is_counted() {
    let mut contract = installed();
    let batch_id = approved(&mut contract, &["aaa"]);
    as_account(operator(), FUNDING);
    contract.open_names(batch_id, names(&["aaa"]));
    as_callback(PromiseResult::Successful(Vec::new()));
    assert!(contract.on_name_opened(batch_id, "aaa".parse().unwrap()));
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
fn a_single_create_that_lands_is_counted() {
    let mut contract = installed();
    as_account(admin(), FUNDING);
    contract
        .create_account("aaa".parse().unwrap(), owner_key())
        .detach();
    as_callback(PromiseResult::Successful(Vec::new()));
    contract.on_account_created("aaa".parse().unwrap());
    assert_eq!(contract.opener_view().opened, 1);
}

#[test]
#[should_panic(expected = "the account was not created")]
fn a_single_create_that_fails_takes_the_whole_call_down() {
    let mut contract = installed();
    as_account(admin(), FUNDING);
    contract
        .create_account("aaa".parse().unwrap(), owner_key())
        .detach();
    as_callback(PromiseResult::Failed);
    contract.on_account_created("aaa".parse().unwrap());
}

#[test]
#[should_panic(expected = "only the admin may call this")]
fn the_operator_cannot_upgrade_the_contract() {
    let mut contract = installed();
    as_account(operator(), YOCTO);
    contract.upgrade(vec![0u8, 1, 2, 3].into()).detach();
}

#[test]
#[should_panic(expected = "only the admin may call this")]
fn a_stranger_cannot_upgrade_the_contract() {
    let mut contract = installed();
    as_account(stranger(), YOCTO);
    contract.upgrade(vec![0u8, 1, 2, 3].into()).detach();
}

#[test]
#[should_panic(expected = "exactly one yoctoNEAR must be attached")]
fn an_upgrade_without_one_yocto_is_refused() {
    let mut contract = installed();
    as_account(admin(), NOTHING);
    contract.upgrade(vec![0u8, 1, 2, 3].into()).detach();
}

#[test]
#[should_panic(expected = "the deploy did not land")]
fn a_deploy_that_fails_is_reported_rather_than_passing_quietly() {
    let mut contract = installed();
    as_callback(PromiseResult::Failed);
    contract.on_upgraded(Base58CryptoHash::from(CryptoHash::default()));
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
fn a_batch_grows_past_any_ceiling_and_costs_one_lookup_to_check() {
    let mut contract = installed();
    as_account(operator(), NOTHING);
    let batch_id = contract.create_batch(owner_key(), FUNDING);
    let mut added = 0u32;
    while added < 1000 {
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
    let batch = contract.get_batch(batch_id).unwrap();
    assert_eq!(batch.count, 1000);
    assert_eq!(batch.remaining, 1000);
    assert!(contract.is_in_batch(batch_id, "name000999".parse().unwrap()));
    assert!(!contract.is_in_batch(batch_id, "name001000".parse().unwrap()));
}
