use near_sdk::json_types::Base58CryptoHash;
use near_sdk::{near, AccountId, NearToken, PublicKey};

use crate::{RegistrarOpener, RegistrarOpenerExt};

const LIST_PAGE_LIMIT: u32 = 200;

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
            next_batch_id: self.next_batch_id,
            live_batches: self.batches.len(),
            opened: self.opened,
            failed: self.failed,
        }
    }
}
