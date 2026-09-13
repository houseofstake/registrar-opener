use near_sdk::{env, require, AccountId, CryptoHash, NearToken, PublicKey};

use crate::error;

const DIGEST_DOMAIN: &[u8] = b"registrar-opener:batch:v1";
const MIN_TLA_LEN: usize = 3;
const MAX_TLA_LEN: usize = 64;

const _: () = assert!(
    MIN_TLA_LEN >= 3,
    "two character top level names are cheap to forge against light clients"
);
const _: () = assert!(
    MAX_TLA_LEN <= 64,
    "the protocol refuses account ids longer than 64 bytes"
);

pub fn to_hash(bytes: Vec<u8>) -> CryptoHash {
    let mut hash = CryptoHash::default();
    hash.copy_from_slice(&bytes);
    hash
}

pub fn seed_digest(owner_key: &PublicKey, funding: NearToken) -> CryptoHash {
    let mut bytes = DIGEST_DOMAIN.to_vec();
    bytes.extend_from_slice(owner_key.as_bytes());
    bytes.extend_from_slice(&funding.as_yoctonear().to_le_bytes());
    to_hash(env::sha256(&bytes))
}

pub fn fold_digest(digest: CryptoHash, name: &AccountId) -> CryptoHash {
    let mut bytes = digest.to_vec();
    bytes.extend_from_slice(name.as_bytes());
    to_hash(env::sha256(&bytes))
}

pub fn assert_openable(names: &[AccountId]) {
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
