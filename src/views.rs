use near_sdk::json_types::Base58CryptoHash;
use near_sdk::{near, AccountId, NearToken, PublicKey};

use crate::{RegistrarOpener, RegistrarOpenerExt};

#[near(serializers = [json])]
pub struct BatchView {
    pub approved: bool,
    pub digest: Base58CryptoHash,
    pub count: u32,
    pub remaining: u32,
    pub owner_key: PublicKey,
    pub funding: NearToken,
}

#[near(serializers = [json])]
pub struct OpenerView {
    pub state_version: u16,
    pub admin: AccountId,
    pub pending_admin: Option<AccountId>,
    pub operator: AccountId,
    pub next_batch_id: u32,
    pub live_batches: u32,
    pub opened: u64,
    pub failed: u64,
}

#[near]
impl RegistrarOpener {
    pub fn get_batch(&self, batch_id: u32) -> Option<BatchView> {
        self.batches.get(&batch_id).map(|batch| BatchView {
            approved: batch.approved,
            digest: Base58CryptoHash::from(batch.digest),
            count: batch.count,
            remaining: batch.remaining,
            owner_key: batch.owner_key.clone(),
            funding: batch.funding,
        })
    }

    pub fn is_in_batch(&self, batch_id: u32, name: AccountId) -> bool {
        self.batches
            .get(&batch_id)
            .is_some_and(|batch| batch.names.contains(&name))
    }

    pub fn opener_view(&self) -> OpenerView {
        OpenerView {
            state_version: self.state_version,
            admin: self.admin.clone(),
            pending_admin: self.pending_admin.clone(),
            operator: self.operator.clone(),
            next_batch_id: self.next_batch_id,
            live_batches: self.batches.len(),
            opened: self.opened,
            failed: self.failed,
        }
    }
}
