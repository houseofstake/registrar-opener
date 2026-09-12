use near_sdk::{env, near, AccountId, Gas, NearToken, Promise, PublicKey};

#[near(contract_state)]
#[derive(Default)]
pub struct Stub {
    marker: String,
}

#[near(serializers = [json])]
pub struct CallerView {
    pub current: AccountId,
    pub predecessor: AccountId,
    pub signer: AccountId,
}

#[near]
impl Stub {
    pub fn set_marker(&mut self, marker: String) {
        self.marker = marker;
    }

    pub fn get_marker(&self) -> String {
        self.marker.clone()
    }

    pub fn who_called(&self) -> CallerView {
        CallerView {
            current: env::current_account_id(),
            predecessor: env::predecessor_account_id(),
            signer: env::signer_account_id(),
        }
    }

    pub fn hop(&self, next: AccountId) -> Promise {
        Promise::new(next).function_call(
            "who_called".to_string(),
            Vec::new(),
            NearToken::from_yoctonear(0),
            Gas::from_tgas(20),
        )
    }

    #[payable]
    pub fn open(&mut self, name: AccountId, owner_key: PublicKey) -> Promise {
        Promise::new(name)
            .create_account()
            .transfer(env::attached_deposit())
            .add_full_access_key(owner_key)
    }
}
