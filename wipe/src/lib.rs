//! Clears everything stored on mainnet `registrar` so `new` can run again, and only when the
//! storage is exactly what the v1.1.0 install left behind.
//!
//! It never lives on `registrar` past the receipt that deploys it. The install is redone as one
//! transaction, which lands whole or not at all:
//!
//! ```text
//!   DeployContract(wipe) -> wipe() -> DeployContract(opener) -> new(admin, operator)
//! ```
//!
//! There are ten rows to clear:
//!
//! ```text
//!   STATE                         the opener, must hash to INSTALLED_STATE_SHA256
//!   \0e ++ u64 index        x4    multisig member set, elements
//!   \0i ++ member           x4    multisig member set, index
//!   \x03 ++ borsh(member json)    multisig request counter of the one member who ever filed
//! ```
//!
//! Every row has to be there to be removed, so a storage that has moved in any way the opener
//! can move it, or that is not the account this was built for, fails the whole transaction.

use near_sdk::{env, require, PublicKey};

/// sha256 of `STATE` on mainnet `registrar` straight after the v1.1.0 install: admin
/// `hos-root.sputnik-dao.near`, operator `root.near`, no nomination, no batch ever drafted.
/// `fixtures/registrar-mainnet-state-v1.1.0.json` holds the bytes.
const INSTALLED_STATE_SHA256: [u8; 32] = [
    0x42, 0xf6, 0xe2, 0xdf, 0x70, 0xdb, 0xa1, 0x72, 0xef, 0xf7, 0xaa, 0x33, 0x1d, 0x17, 0xf2, 0xb9,
    0x97, 0x62, 0xc5, 0x91, 0xa6, 0xe5, 0x6d, 0xdd, 0x96, 0xf9, 0xde, 0x10, 0xf0, 0xa5, 0x71, 0x6d,
];

/// The multisig's members in element order, as its member set stored them.
const MULTISIG_MEMBERS: [&str; 4] = [
    "ed25519:BFVZgNSbUf3rVcwFww7vbXXfWy38VLgpv3PdPFbHotkP",
    "ed25519:LjaV16AvozjU8ZFbAFHBQnVzuhCDotM3mtPU2kuqQrG",
    "ed25519:9gGJF6M36oNiUb2cGACGc6BdRBD6f5QDxPZeMpyrj9X3",
    "ed25519:4BGbi2xFEp7hBsfGGLsrzB2DY2VTaADxh4KdpqdCsgSf",
];

/// The request counter is keyed by the member rendered as JSON, not by its borsh bytes.
const MULTISIG_REQUEST_COUNTER_OWNER: &str =
    r#"{"public_key":"ed25519:4BGbi2xFEp7hBsfGGLsrzB2DY2VTaADxh4KdpqdCsgSf"}"#;

pub const ONLY_SELF: &str = "only this account may call this";
pub const NOT_THE_INSTALL: &str = "state is not the untouched install this wipe was built for";
pub const ROW_MISSING: &str = "a multisig row this wipe was built for is not there";

#[no_mangle]
pub extern "C" fn wipe() {
    clear_install();
}

fn clear_install() {
    require!(
        env::predecessor_account_id() == env::current_account_id(),
        ONLY_SELF
    );
    let state = env::storage_read(b"STATE").unwrap_or_else(|| env::panic_str(NOT_THE_INSTALL));
    require!(
        env::sha256_array(&state) == INSTALLED_STATE_SHA256,
        NOT_THE_INSTALL
    );
    env::storage_remove(b"STATE");
    for key in multisig_rows() {
        require!(env::storage_remove(&key), ROW_MISSING);
    }
}

fn multisig_rows() -> Vec<Vec<u8>> {
    let mut rows = Vec::with_capacity(MULTISIG_MEMBERS.len() * 2 + 1);
    for (index, raw) in (0u64..).zip(MULTISIG_MEMBERS) {
        let key: PublicKey = raw
            .parse()
            .unwrap_or_else(|_| env::panic_str("a multisig member key does not parse"));
        let mut element = b"\0e".to_vec();
        element.extend_from_slice(&index.to_le_bytes());
        rows.push(element);

        let mut member = vec![0u8];
        member.extend_from_slice(&(key.as_bytes().len() as u32).to_le_bytes());
        member.extend_from_slice(key.as_bytes());
        let mut position = b"\0i".to_vec();
        position.extend_from_slice(&member);
        rows.push(position);
    }
    let mut counter = vec![3u8];
    counter.extend_from_slice(&(MULTISIG_REQUEST_COUNTER_OWNER.len() as u32).to_le_bytes());
    counter.extend_from_slice(MULTISIG_REQUEST_COUNTER_OWNER.as_bytes());
    rows.push(counter);
    rows
}

#[cfg(test)]
mod tests {
    use near_sdk::json_types::Base64VecU8;
    use near_sdk::serde::Deserialize;
    use near_sdk::test_utils::VMContextBuilder;
    use near_sdk::{env, serde_json, testing_env, AccountId};

    use crate::{clear_install, multisig_rows, INSTALLED_STATE_SHA256};

    #[derive(Deserialize)]
    #[serde(crate = "near_sdk::serde")]
    struct Row {
        key: Base64VecU8,
        value: Base64VecU8,
    }

    fn mainnet_rows() -> Vec<(Vec<u8>, Vec<u8>)> {
        let raw = include_str!("../../fixtures/registrar-mainnet-state-v1.1.0.json");
        let rows: Vec<Row> = serde_json::from_str(raw).unwrap();
        rows.into_iter()
            .map(|row| (row.key.0, row.value.0))
            .collect()
    }

    fn called_by(predecessor: &str) {
        let registrar: AccountId = "registrar".parse().unwrap();
        testing_env!(VMContextBuilder::new()
            .current_account_id(registrar)
            .predecessor_account_id(predecessor.parse().unwrap())
            .build());
    }

    fn load_mainnet() {
        for (key, value) in mainnet_rows() {
            env::storage_write(&key, &value);
        }
    }

    #[test]
    fn the_constant_is_the_hash_of_mainnets_installed_state() {
        called_by("registrar");
        let state = mainnet_rows()
            .into_iter()
            .find(|(key, _)| key == b"STATE")
            .map(|(_, value)| value)
            .unwrap();
        assert_eq!(env::sha256_array(state), INSTALLED_STATE_SHA256);
    }

    #[test]
    fn the_rows_it_clears_are_exactly_mainnets_rows() {
        called_by("registrar");
        let mut expected: Vec<Vec<u8>> = mainnet_rows()
            .into_iter()
            .map(|(key, _)| key)
            .filter(|key| key != b"STATE")
            .collect();
        let mut ours = multisig_rows();
        expected.sort();
        ours.sort();
        assert_eq!(ours, expected);
    }

    #[test]
    fn mainnets_storage_is_cleared_to_nothing() {
        called_by("registrar");
        load_mainnet();
        clear_install();
        for (key, _) in mainnet_rows() {
            assert!(!env::storage_has_key(&key), "{key:?} survived the wipe");
        }
    }

    #[test]
    #[should_panic(expected = "only this account may call this")]
    fn a_stranger_cannot_wipe() {
        called_by("stranger.near");
        load_mainnet();
        clear_install();
    }

    #[test]
    #[should_panic(expected = "state is not the untouched install this wipe was built for")]
    fn a_state_that_moved_by_one_byte_is_refused() {
        called_by("registrar");
        load_mainnet();
        let mut moved = env::storage_read(b"STATE").unwrap();
        let last = moved.len() - 1;
        moved[last] = 1;
        env::storage_write(b"STATE", &moved);
        clear_install();
    }

    #[test]
    #[should_panic(expected = "state is not the untouched install this wipe was built for")]
    fn an_account_without_state_is_refused() {
        called_by("registrar");
        clear_install();
    }

    #[test]
    #[should_panic(expected = "a multisig row this wipe was built for is not there")]
    fn a_missing_multisig_row_is_refused() {
        called_by("registrar");
        load_mainnet();
        env::storage_remove(b"\0e\x03\0\0\0\0\0\0\0");
        clear_install();
    }
}
