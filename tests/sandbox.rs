use anyhow::{Context, Result};
use base64::Engine;
use near_workspaces::network::Sandbox;
use near_workspaces::types::{Gas, KeyType, NearToken, PublicKey, SecretKey};
use near_workspaces::{Account, Contract, Worker};
use serde_json::json;
use sha2::{Digest, Sha256};

const FUNDING: NearToken = NearToken::from_millinear(10);
const BOND: NearToken = NearToken::from_near(1);
const YOCTO: NearToken = NearToken::from_yoctonear(1);
const COUNCIL_SIZE: usize = 5;
const COUNCIL_THRESHOLD: usize = 3;
const SANDBOX_DELAY_NS: u64 = 2_000_000_000;
const DIGEST_DOMAIN: &[u8] = b"registrar-opener:batch:v1";

struct Fleet {
    worker: Worker<Sandbox>,
    registrar: Contract,
    dao: Contract,
    council: Vec<Account>,
    operator: Account,
    stranger: Account,
    owner_key: PublicKey,
}

fn fixture(name: &str) -> Result<Vec<u8>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    std::fs::read(&path).with_context(|| format!("read {}", path.display()))
}

fn mainnet_state() -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let raw = fixture("registrar-mainnet-state.json")?;
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&raw)?;
    rows.iter()
        .map(|row| {
            let key = row["key"].as_str().context("state row has no key")?;
            let value = row["value"].as_str().context("state row has no value")?;
            Ok((
                base64::engine::general_purpose::STANDARD.decode(key)?,
                base64::engine::general_purpose::STANDARD.decode(value)?,
            ))
        })
        .collect()
}

fn member_bytes(key: &PublicKey) -> Vec<u8> {
    let raw = key_bytes(key);
    let mut out = vec![0u8];
    out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    out.extend_from_slice(&raw);
    out
}

fn element_key(index: u64) -> Vec<u8> {
    let mut key = b"\0e".to_vec();
    key.extend_from_slice(&index.to_le_bytes());
    key
}

fn index_key(member: &[u8]) -> Vec<u8> {
    let mut key = b"\0i".to_vec();
    key.extend_from_slice(member);
    key
}

fn with_member_count(state: &[u8], count: u64) -> Result<Vec<u8>> {
    let prefix_len = u32::from_le_bytes(state[0..4].try_into()?) as usize;
    let at = 4 + prefix_len;
    let mut out = state.to_vec();
    out[at..at + 8].copy_from_slice(&count.to_le_bytes());
    Ok(out)
}

fn member_count(state: &[u8]) -> Result<u64> {
    let prefix_len = u32::from_le_bytes(state[0..4].try_into()?) as usize;
    let at = 4 + prefix_len;
    Ok(u64::from_le_bytes(state[at..at + 8].try_into()?))
}

async fn patch_mainnet_registrar(worker: &Worker<Sandbox>, sk: &SecretKey) -> Result<Contract> {
    let id: near_workspaces::AccountId = "registrar".parse()?;
    let rows = mainnet_state()?;
    let mut patch = worker
        .patch(&id)
        .code(&fixture("registrar-mainnet.wasm")?)
        .access_key(sk.public_key(), near_workspaces::AccessKey::full_access())
        .account(
            near_workspaces::AccountDetailsPatch::default().balance(NearToken::from_near(100_000)),
        );
    for (key, value) in &rows {
        patch = patch.state(key, value);
    }
    patch.transact().await?;
    Ok(Contract::from_secret_key(id, sk.clone(), worker))
}

fn build_dir() -> Result<std::path::PathBuf> {
    let out = std::process::Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .context("run cargo metadata")?;
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let dir = meta["target_directory"]
        .as_str()
        .context("cargo metadata reported no target_directory")?;
    Ok(std::path::PathBuf::from(dir))
}

fn ours_wasm() -> Result<Vec<u8>> {
    let path = build_dir()?.join("near").join("registrar_opener.wasm");
    std::fs::read(&path).with_context(|| {
        format!(
            "{} is missing, run cargo near build non-reproducible-wasm --locked --no-abi",
            path.display()
        )
    })
}

fn stub_wasm() -> Result<Vec<u8>> {
    let path = build_dir()?
        .join("wasm32-unknown-unknown")
        .join("release")
        .join("registrar_opener_stub.wasm");
    std::fs::read(&path).with_context(|| {
        format!(
            "{} is missing, run cargo build -p registrar-opener-stub \
             --target wasm32-unknown-unknown --release",
            path.display()
        )
    })
}

fn key_bytes(key: &PublicKey) -> Vec<u8> {
    let text = key.to_string();
    let encoded = text.split(':').next_back().unwrap_or_default();
    let mut bytes = vec![0u8];
    bytes.extend_from_slice(&bs58::decode(encoded).into_vec().unwrap_or_default());
    bytes
}

fn expected_digest(owner_key: &PublicKey, funding: NearToken, names: &[&str]) -> [u8; 32] {
    let mut seed = DIGEST_DOMAIN.to_vec();
    seed.extend_from_slice(&key_bytes(owner_key));
    seed.extend_from_slice(&funding.as_yoctonear().to_le_bytes());
    let mut digest: [u8; 32] = Sha256::digest(&seed).into();
    for name in names {
        let mut step = digest.to_vec();
        step.extend_from_slice(name.as_bytes());
        digest = Sha256::digest(&step).into();
    }
    digest
}

async fn patch_registrar(
    worker: &Worker<Sandbox>,
    wasm: &[u8],
    sk: &SecretKey,
) -> Result<Contract> {
    let id: near_workspaces::AccountId = "registrar".parse()?;
    worker
        .patch(&id)
        .code(wasm)
        .access_key(sk.public_key(), near_workspaces::AccessKey::full_access())
        .account(
            near_workspaces::AccountDetailsPatch::default().balance(NearToken::from_near(100_000)),
        )
        .transact()
        .await?;
    Ok(Contract::from_secret_key(id, sk.clone(), worker))
}

async fn deploy_dao(worker: &Worker<Sandbox>, council: &[Account]) -> Result<Contract> {
    let dao = worker
        .dev_deploy(&fixture("sputnik-dao-v2.3.1.wasm")?)
        .await?;
    let members: Vec<&str> = council.iter().map(|member| member.id().as_str()).collect();
    dao.call("new")
        .args_json(json!({
            "config": { "name": "opener", "purpose": "open top level names", "metadata": "" },
            "policy": members,
        }))
        .max_gas()
        .transact()
        .await?
        .into_result()?;
    Ok(dao)
}

async fn install_opener(dao: &Contract, registrar: &Contract, operator: &Account) -> Result<()> {
    registrar
        .call("new")
        .args_json(json!({
            "admin": dao.id(),
            "operator": operator.id(),
            "upgrade_delay_ns": SANDBOX_DELAY_NS.to_string(),
        }))
        .max_gas()
        .transact()
        .await?
        .into_result()?;
    Ok(())
}

async fn setup() -> Result<Fleet> {
    let worker = near_workspaces::sandbox().await?;
    let sk = SecretKey::from_seed(KeyType::ED25519, "registrar-opener");
    let registrar = patch_registrar(&worker, &ours_wasm()?, &sk).await?;

    let mut council = Vec::new();
    for _ in 0..COUNCIL_SIZE {
        council.push(worker.dev_create_account().await?);
    }
    let dao = deploy_dao(&worker, &council).await?;
    let operator = worker.dev_create_account().await?;
    let stranger = worker.dev_create_account().await?;
    install_opener(&dao, &registrar, &operator).await?;

    let owner_key = SecretKey::from_seed(KeyType::ED25519, "cohort-owner").public_key();
    Ok(Fleet {
        worker,
        registrar,
        dao,
        council,
        operator,
        stranger,
        owner_key,
    })
}

async fn draft(fleet: &Fleet, names: &[&str]) -> Result<u32> {
    let batch_id: u32 = fleet
        .operator
        .call(fleet.registrar.id(), "create_batch")
        .args_json(json!({ "owner_key": fleet.owner_key, "funding": FUNDING }))
        .max_gas()
        .transact()
        .await?
        .json()?;
    fleet
        .operator
        .call(fleet.registrar.id(), "add_names")
        .args_json(json!({ "batch_id": batch_id, "names": names }))
        .max_gas()
        .transact()
        .await?
        .into_result()?;
    Ok(batch_id)
}

async fn batch_view(fleet: &Fleet, batch_id: u32) -> Result<serde_json::Value> {
    Ok(fleet
        .registrar
        .view("get_batch")
        .args_json(json!({ "batch_id": batch_id }))
        .await?
        .json()?)
}

async fn digest_of(fleet: &Fleet, batch_id: u32) -> Result<String> {
    Ok(batch_view(fleet, batch_id).await?["digest"]
        .as_str()
        .context("the batch carries no digest")?
        .to_string())
}

async fn dao_calls(
    fleet: &Fleet,
    method: &str,
    args: serde_json::Value,
    deposit: NearToken,
    votes: usize,
) -> Result<u64> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&args)?);
    let proposal_id: u64 = fleet.council[0]
        .call(fleet.dao.id(), "add_proposal")
        .args_json(json!({
            "proposal": {
                "description": format!("call {method} on registrar"),
                "kind": {
                    "FunctionCall": {
                        "receiver_id": fleet.registrar.id(),
                        "actions": [{
                            "method_name": method,
                            "args": encoded,
                            "deposit": deposit.as_yoctonear().to_string(),
                            "gas": Gas::from_tgas(100).as_gas().to_string(),
                        }],
                    }
                }
            }
        }))
        .deposit(BOND)
        .max_gas()
        .transact()
        .await?
        .json()?;
    for member in fleet.council.iter().take(votes) {
        member
            .call(fleet.dao.id(), "act_proposal")
            .args_json(json!({ "id": proposal_id, "action": "VoteApprove" }))
            .max_gas()
            .transact()
            .await?
            .into_result()?;
    }
    Ok(proposal_id)
}

async fn approve(fleet: &Fleet, batch_id: u32) -> Result<u64> {
    let digest = digest_of(fleet, batch_id).await?;
    dao_calls(
        fleet,
        "approve_batch",
        json!({ "batch_id": batch_id, "digest": digest }),
        YOCTO,
        COUNCIL_THRESHOLD,
    )
    .await
}

async fn open(
    fleet: &Fleet,
    batch_id: u32,
    names: &[&str],
    gas: Gas,
) -> Result<near_workspaces::result::ExecutionFinalResult> {
    let total = NearToken::from_yoctonear(FUNDING.as_yoctonear() * names.len() as u128);
    Ok(fleet
        .operator
        .call(fleet.registrar.id(), "open_names")
        .args_json(json!({ "batch_id": batch_id, "names": names }))
        .deposit(total)
        .gas(gas)
        .transact()
        .await?)
}

async fn assert_opened(fleet: &Fleet, name: &str) -> Result<()> {
    let id: near_workspaces::AccountId = name.parse()?;
    let account = fleet.worker.view_account(&id).await?;
    let floor = account.storage_usage as u128 * 10u128.pow(19);
    assert!(
        account.balance.as_yoctonear() > floor,
        "{name} is below its own storage floor"
    );
    let keys = fleet.worker.view_access_keys(&id).await?;
    assert_eq!(keys.len(), 1, "{name} should carry exactly the owner key");
    Ok(())
}

#[tokio::test]
async fn the_multisig_installs_the_opener_over_itself_in_one_request() -> Result<()> {
    let worker = near_workspaces::sandbox().await?;
    let sk = SecretKey::from_seed(KeyType::ED25519, "registrar-opener");
    let registrar = patch_registrar(&worker, &fixture("registrar-mainnet.wasm")?, &sk).await?;

    let alice = worker.dev_create_account().await?;
    let bob = worker.dev_create_account().await?;
    registrar
        .call("new")
        .args_json(json!({
            "members": [{ "account_id": alice.id() }, { "account_id": bob.id() }],
            "num_confirmations": 2,
        }))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    let members: Vec<serde_json::Value> = registrar.view("get_members").await?.json()?;
    assert_eq!(members.len(), 2, "the multisig did not come up");

    let council = worker.dev_create_account().await?;
    let operator = worker.dev_create_account().await?;
    let init = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&json!({
        "admin": council.id(),
        "operator": operator.id(),
        "upgrade_delay_ns": SANDBOX_DELAY_NS.to_string(),
    }))?);
    let request_id: u32 = alice
        .call(registrar.id(), "add_request_and_confirm")
        .args_json(json!({
            "request": {
                "receiver_id": registrar.id(),
                "actions": [
                    {
                        "type": "DeployContract",
                        "code": base64::engine::general_purpose::STANDARD.encode(ours_wasm()?),
                    },
                    {
                        "type": "FunctionCall",
                        "method_name": "new",
                        "args": init,
                        "deposit": "0",
                        "gas": Gas::from_tgas(50).as_gas().to_string(),
                    },
                ],
            }
        }))
        .max_gas()
        .transact()
        .await?
        .json()?;

    bob.call(registrar.id(), "confirm")
        .args_json(json!({ "request_id": request_id }))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    let view: serde_json::Value = registrar.view("opener_view").await?.json()?;
    assert_eq!(view["admin"], council.id().as_str());
    assert_eq!(view["operator"], operator.id().as_str());
    assert_eq!(view["state_version"], 1);
    assert!(
        registrar.view("get_members").await.is_err(),
        "the multisig methods should be gone once the code is replaced"
    );
    Ok(())
}

#[tokio::test]
async fn the_multisig_installs_the_opener_from_mainnets_own_state_at_its_own_threshold(
) -> Result<()> {
    let worker = near_workspaces::sandbox().await?;
    let sk = SecretKey::from_seed(KeyType::ED25519, "registrar-opener");
    let id: near_workspaces::AccountId = "registrar".parse()?;

    let seats: Vec<SecretKey> = ["seat-one", "seat-two"]
        .iter()
        .map(|name| SecretKey::from_seed(KeyType::ED25519, name))
        .collect();

    let rows = mainnet_state()?;
    let state = rows
        .iter()
        .find(|(key, _)| key == b"STATE")
        .map(|(_, value)| value.clone())
        .context("mainnet state has no STATE row")?;
    let present = member_count(&state)?;
    assert_eq!(present, 4, "mainnet carries four members");

    let live: PublicKey = "ed25519:BFVZgNSbUf3rVcwFww7vbXXfWy38VLgpv3PdPFbHotkP".parse()?;
    let ours = index_key(&member_bytes(&live));
    assert!(
        rows.iter().any(|(key, _)| key == &ours),
        "our member encoding does not match mainnet's own index rows"
    );

    let mut patch = worker
        .patch(&id)
        .code(&fixture("registrar-mainnet.wasm")?)
        .access_key(sk.public_key(), near_workspaces::AccessKey::full_access())
        .account(
            near_workspaces::AccountDetailsPatch::default().balance(NearToken::from_near(100_000)),
        );
    for seat in &seats {
        patch = patch.access_key(seat.public_key(), near_workspaces::AccessKey::full_access());
    }
    for (key, value) in &rows {
        if key == b"STATE" {
            continue;
        }
        patch = patch.state(key, value);
    }
    let grown = with_member_count(&state, present + seats.len() as u64)?;
    patch = patch.state(b"STATE", &grown);
    for (offset, seat) in seats.iter().enumerate() {
        let index = present + offset as u64;
        let member = member_bytes(&seat.public_key());
        patch = patch.state(&element_key(index), &member);
        patch = patch.state(&index_key(&member), &index.to_le_bytes());
    }
    patch.transact().await?;
    let registrar = Contract::from_secret_key(id.clone(), sk.clone(), &worker);

    let holders: Vec<Contract> = seats
        .iter()
        .map(|seat| Contract::from_secret_key(id.clone(), seat.clone(), &worker))
        .collect();

    let members: Vec<serde_json::Value> = registrar.view("get_members").await?.json()?;
    assert_eq!(members.len(), 6, "the appended seats did not take");
    let keys: Vec<&str> = members
        .iter()
        .filter_map(|member| member["public_key"].as_str())
        .collect();
    assert_eq!(
        &keys[..4],
        &[
            "ed25519:BFVZgNSbUf3rVcwFww7vbXXfWy38VLgpv3PdPFbHotkP",
            "ed25519:LjaV16AvozjU8ZFbAFHBQnVzuhCDotM3mtPU2kuqQrG",
            "ed25519:9gGJF6M36oNiUb2cGACGc6BdRBD6f5QDxPZeMpyrj9X3",
            "ed25519:4BGbi2xFEp7hBsfGGLsrzB2DY2VTaADxh4KdpqdCsgSf",
        ],
        "mainnet's own four members were disturbed"
    );
    assert_eq!(
        &keys[4..],
        &[
            seats[0].public_key().to_string().as_str(),
            seats[1].public_key().to_string().as_str(),
        ],
        "the appended seats are not the keys we hold"
    );
    let threshold: u32 = registrar.view("get_num_confirmations").await?.json()?;
    assert_eq!(
        threshold, 2,
        "the threshold did not come from mainnet's blob"
    );

    let council = worker.dev_create_account().await?;
    let operator = worker.dev_create_account().await?;
    let init = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&json!({
        "admin": council.id(),
        "operator": operator.id(),
        "upgrade_delay_ns": SANDBOX_DELAY_NS.to_string(),
    }))?);
    let request_id: u32 = holders[0]
        .call("add_request_and_confirm")
        .args_json(json!({
            "request": {
                "receiver_id": registrar.id(),
                "actions": [
                    {
                        "type": "DeployContract",
                        "code": base64::engine::general_purpose::STANDARD.encode(ours_wasm()?),
                    },
                    {
                        "type": "FunctionCall",
                        "method_name": "new",
                        "args": init,
                        "deposit": "0",
                        "gas": Gas::from_tgas(50).as_gas().to_string(),
                    },
                ],
            }
        }))
        .max_gas()
        .transact()
        .await?
        .json()?;
    assert_eq!(
        request_id, 1,
        "the nonce did not continue from mainnet's own request_nonce"
    );

    holders[1]
        .call("confirm")
        .args_json(json!({ "request_id": request_id }))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    let view: serde_json::Value = registrar.view("opener_view").await?.json()?;
    assert_eq!(view["admin"], council.id().as_str());
    assert_eq!(view["operator"], operator.id().as_str());
    assert!(
        registrar.view("get_members").await.is_err(),
        "the multisig survived its own replacement request"
    );
    Ok(())
}

#[tokio::test]
async fn mainnets_own_stored_state_reads_back_under_mainnets_own_code() -> Result<()> {
    let worker = near_workspaces::sandbox().await?;
    let sk = SecretKey::from_seed(KeyType::ED25519, "registrar-opener");
    let registrar = patch_mainnet_registrar(&worker, &sk).await?;

    let members: Vec<serde_json::Value> = registrar.view("get_members").await?.json()?;
    let keys: Vec<&str> = members
        .iter()
        .filter_map(|member| member["public_key"].as_str())
        .collect();
    assert_eq!(
        keys,
        vec![
            "ed25519:BFVZgNSbUf3rVcwFww7vbXXfWy38VLgpv3PdPFbHotkP",
            "ed25519:LjaV16AvozjU8ZFbAFHBQnVzuhCDotM3mtPU2kuqQrG",
            "ed25519:9gGJF6M36oNiUb2cGACGc6BdRBD6f5QDxPZeMpyrj9X3",
            "ed25519:4BGbi2xFEp7hBsfGGLsrzB2DY2VTaADxh4KdpqdCsgSf",
        ],
        "the replayed state did not reproduce mainnet's members"
    );

    let threshold: u32 = registrar.view("get_num_confirmations").await?.json()?;
    assert_eq!(threshold, 2, "mainnet runs a 2 of 4");
    let pending: Vec<u32> = registrar.view("list_request_ids").await?.json()?;
    assert!(pending.is_empty(), "mainnet has nothing in flight");
    Ok(())
}

#[tokio::test]
async fn the_opener_installs_over_mainnets_own_stored_state() -> Result<()> {
    let worker = near_workspaces::sandbox().await?;
    let sk = SecretKey::from_seed(KeyType::ED25519, "registrar-opener");
    let registrar = patch_mainnet_registrar(&worker, &sk).await?;

    let before = worker.view_state(registrar.id()).await?;
    assert_eq!(before.len(), 10, "mainnet carries ten rows");

    let mut council = Vec::new();
    for _ in 0..COUNCIL_SIZE {
        council.push(worker.dev_create_account().await?);
    }
    let dao = deploy_dao(&worker, &council).await?;
    let operator = worker.dev_create_account().await?;
    let stranger = worker.dev_create_account().await?;

    let install = registrar
        .as_account()
        .batch(registrar.id())
        .deploy(&ours_wasm()?)
        .call(
            near_workspaces::operations::Function::new("new")
                .args_json(json!({
                    "admin": dao.id(),
                    "operator": operator.id(),
                    "upgrade_delay_ns": SANDBOX_DELAY_NS.to_string(),
                }))
                .gas(Gas::from_tgas(50)),
        )
        .transact()
        .await?;
    assert!(
        install.is_success(),
        "installing over mainnet state failed: {install:#?}"
    );

    let view: serde_json::Value = registrar.view("opener_view").await?.json()?;
    assert_eq!(view["admin"], dao.id().as_str());
    assert_eq!(view["operator"], operator.id().as_str());
    assert_eq!(view["state_version"], 1);
    assert!(
        registrar.view("get_members").await.is_err(),
        "the multisig survived the replacement"
    );

    let after = worker.view_state(registrar.id()).await?;
    assert_eq!(
        after.len(),
        10,
        "the multisig's own rows are orphaned in place, not cleared, so the count holds"
    );
    assert_ne!(
        before.get(b"STATE".as_slice()),
        after.get(b"STATE".as_slice()),
        "STATE was not replaced"
    );

    let fleet = Fleet {
        worker,
        registrar,
        dao,
        council,
        operator,
        stranger,
        owner_key: SecretKey::from_seed(KeyType::ED25519, "cohort-owner").public_key(),
    };
    let batch_id = draft(&fleet, &["alpha"]).await?;
    approve(&fleet, batch_id).await?;
    open(&fleet, batch_id, &["alpha"], Gas::from_tgas(300))
        .await?
        .into_result()?;
    assert_opened(&fleet, "alpha").await?;
    Ok(())
}

#[tokio::test]
async fn the_dao_is_the_caller_the_contract_sees_when_a_proposal_executes() -> Result<()> {
    let fleet = setup().await?;
    let batch_id = draft(&fleet, &["alpha", "bravo"]).await?;
    assert_eq!(batch_view(&fleet, batch_id).await?["approved"], false);

    approve(&fleet, batch_id).await?;

    let batch = batch_view(&fleet, batch_id).await?;
    assert_eq!(
        batch["approved"], true,
        "the DAO vote did not land: {batch}"
    );
    assert_eq!(batch["count"], 2);
    assert_eq!(batch["remaining"], 2);
    Ok(())
}

#[tokio::test]
async fn fewer_votes_than_the_threshold_leave_the_batch_unapproved() -> Result<()> {
    let fleet = setup().await?;
    let batch_id = draft(&fleet, &["alpha"]).await?;
    let digest = digest_of(&fleet, batch_id).await?;
    dao_calls(
        &fleet,
        "approve_batch",
        json!({ "batch_id": batch_id, "digest": digest }),
        YOCTO,
        COUNCIL_THRESHOLD - 1,
    )
    .await?;
    assert_eq!(
        batch_view(&fleet, batch_id).await?["approved"],
        false,
        "the batch was approved on fewer votes than the threshold"
    );
    Ok(())
}

#[tokio::test]
async fn an_approved_batch_opens_real_top_level_accounts() -> Result<()> {
    let fleet = setup().await?;
    let batch_id = draft(&fleet, &["alpha", "bravo"]).await?;
    approve(&fleet, batch_id).await?;

    let outcome = open(&fleet, batch_id, &["alpha", "bravo"], Gas::from_tgas(300)).await?;
    assert!(outcome.is_success(), "open_names failed: {outcome:#?}");

    assert_opened(&fleet, "alpha").await?;
    assert_opened(&fleet, "bravo").await?;
    assert_eq!(batch_view(&fleet, batch_id).await?["remaining"], 0);
    let view: serde_json::Value = fleet.registrar.view("opener_view").await?.json()?;
    assert_eq!(view["opened"], 2);
    assert_eq!(view["failed"], 0);
    Ok(())
}

#[tokio::test]
async fn the_stored_digest_is_the_one_an_outside_observer_computes() -> Result<()> {
    let fleet = setup().await?;
    let batch_id = draft(&fleet, &["alpha", "bravo", "charlie"]).await?;
    let stored = digest_of(&fleet, batch_id).await?;
    let ours = expected_digest(&fleet.owner_key, FUNDING, &["alpha", "bravo", "charlie"]);
    assert_eq!(
        stored,
        bs58::encode(ours).into_string(),
        "the council cannot verify a batch it cannot recompute"
    );
    Ok(())
}

#[tokio::test]
async fn a_name_outside_the_approved_batch_cannot_be_opened() -> Result<()> {
    let fleet = setup().await?;
    let batch_id = draft(&fleet, &["alpha"]).await?;
    approve(&fleet, batch_id).await?;

    let outcome = open(&fleet, batch_id, &["charlie"], Gas::from_tgas(300)).await?;
    assert!(
        outcome.is_failure(),
        "a name the council never saw was opened"
    );
    assert!(
        fleet
            .worker
            .view_account(&"charlie".parse()?)
            .await
            .is_err(),
        "charlie exists despite the refusal"
    );
    Ok(())
}

#[tokio::test]
async fn replacing_the_operator_strands_an_approved_batch() -> Result<()> {
    let fleet = setup().await?;
    let batch_id = draft(&fleet, &["alpha"]).await?;
    approve(&fleet, batch_id).await?;

    dao_calls(
        &fleet,
        "change_operator",
        json!({ "operator": fleet.stranger.id() }),
        YOCTO,
        COUNCIL_THRESHOLD,
    )
    .await?;

    let view: serde_json::Value = fleet.registrar.view("opener_view").await?.json()?;
    assert_eq!(view["operator"], fleet.stranger.id().as_str());
    assert_eq!(view["operator_epoch"], 1);
    assert_eq!(batch_view(&fleet, batch_id).await?["stale"], true);

    let outcome = fleet
        .stranger
        .call(fleet.registrar.id(), "open_names")
        .args_json(json!({ "batch_id": batch_id, "names": ["alpha"] }))
        .deposit(FUNDING)
        .max_gas()
        .transact()
        .await?;
    assert!(
        outcome.is_failure(),
        "a replaced operator's approved batch was still openable"
    );
    Ok(())
}

#[tokio::test]
async fn the_documented_per_call_maximum_fits_and_every_name_lands() -> Result<()> {
    let fleet = setup().await?;
    let names: Vec<String> = (0..20).map(|index| format!("batch{index:02}")).collect();
    let borrowed: Vec<&str> = names.iter().map(String::as_str).collect();
    let batch_id = draft(&fleet, &borrowed).await?;
    approve(&fleet, batch_id).await?;

    let outcome = open(&fleet, batch_id, &borrowed, Gas::from_tgas(300)).await?;
    assert!(
        outcome.is_success(),
        "a full batch did not fit one transaction: {outcome:#?}"
    );
    for name in &borrowed {
        assert_opened(&fleet, name).await?;
    }
    assert_eq!(batch_view(&fleet, batch_id).await?["remaining"], 0);
    Ok(())
}

#[tokio::test]
async fn a_full_add_names_call_fits_and_the_digest_still_matches() -> Result<()> {
    let fleet = setup().await?;
    let names: Vec<String> = (0..100).map(|index| format!("cohort{index:03}")).collect();
    let borrowed: Vec<&str> = names.iter().map(String::as_str).collect();

    let batch_id: u32 = fleet
        .operator
        .call(fleet.registrar.id(), "create_batch")
        .args_json(json!({ "owner_key": fleet.owner_key, "funding": FUNDING }))
        .max_gas()
        .transact()
        .await?
        .json()?;
    let outcome = fleet
        .operator
        .call(fleet.registrar.id(), "add_names")
        .args_json(json!({ "batch_id": batch_id, "names": borrowed }))
        .max_gas()
        .transact()
        .await?;
    assert!(
        outcome.is_success(),
        "a full add_names call did not fit one transaction: {outcome:#?}"
    );

    let batch = batch_view(&fleet, batch_id).await?;
    assert_eq!(batch["count"], 100);
    assert_eq!(
        digest_of(&fleet, batch_id).await?,
        bs58::encode(expected_digest(&fleet.owner_key, FUNDING, &borrowed)).into_string()
    );
    Ok(())
}

#[tokio::test]
async fn a_name_that_already_exists_returns_its_slot_to_the_batch() -> Result<()> {
    let fleet = setup().await?;
    let first = draft(&fleet, &["alpha"]).await?;
    approve(&fleet, first).await?;
    open(&fleet, first, &["alpha"], Gas::from_tgas(300))
        .await?
        .into_result()?;
    assert_opened(&fleet, "alpha").await?;

    let second = draft(&fleet, &["alpha"]).await?;
    approve(&fleet, second).await?;
    let outcome = open(&fleet, second, &["alpha"], Gas::from_tgas(300)).await?;
    assert!(
        outcome.is_success(),
        "the call should succeed and the create should fail in its own receipt"
    );
    assert_eq!(
        batch_view(&fleet, second).await?["remaining"],
        1,
        "the slot was not returned after the create failed"
    );
    let view: serde_json::Value = fleet.registrar.view("opener_view").await?.json()?;
    assert_eq!(view["failed"], 1);
    Ok(())
}

#[tokio::test]
async fn the_admin_can_open_one_name_outside_any_batch() -> Result<()> {
    let fleet = setup().await?;
    dao_calls(
        &fleet,
        "create_account",
        json!({ "name": "solo", "owner_key": fleet.owner_key }),
        FUNDING,
        COUNCIL_THRESHOLD,
    )
    .await?;
    assert_opened(&fleet, "solo").await?;
    Ok(())
}

#[tokio::test]
async fn an_upgrade_lands_only_after_the_delay_and_carries_the_state_across() -> Result<()> {
    let fleet = setup().await?;
    let batch_id = draft(&fleet, &["alpha"]).await?;
    approve(&fleet, batch_id).await?;

    let code = ours_wasm()?;
    let hash = bs58::encode(Sha256::digest(&code)).into_string();
    dao_calls(
        &fleet,
        "approve_code",
        json!({ "code_hash": hash }),
        YOCTO,
        COUNCIL_THRESHOLD,
    )
    .await?;

    let encoded = base64::engine::general_purpose::STANDARD.encode(&code);
    let early = fleet
        .operator
        .call(fleet.registrar.id(), "upgrade")
        .args_json(json!({ "code": encoded }))
        .max_gas()
        .transact()
        .await?;
    assert!(
        early.is_failure(),
        "the upgrade landed inside the approval delay"
    );

    fleet.worker.fast_forward(10).await?;

    let late = fleet
        .operator
        .call(fleet.registrar.id(), "upgrade")
        .args_json(json!({ "code": encoded }))
        .max_gas()
        .transact()
        .await?;
    assert!(late.is_success(), "the upgrade failed: {late:#?}");

    let view: serde_json::Value = fleet.registrar.view("opener_view").await?.json()?;
    assert_eq!(view["operator"], fleet.operator.id().as_str());
    assert!(view["pending_code"].is_null(), "the approval was not spent");
    let batch = batch_view(&fleet, batch_id).await?;
    assert_eq!(batch["approved"], true, "the batch did not survive migrate");
    assert_eq!(batch["remaining"], 1);
    Ok(())
}

#[tokio::test]
async fn a_failed_upgrade_leaves_the_approval_standing_for_a_retry() -> Result<()> {
    let fleet = setup().await?;
    let junk = b"this is not a wasm module".to_vec();
    let hash = bs58::encode(Sha256::digest(&junk)).into_string();
    dao_calls(
        &fleet,
        "approve_code",
        json!({ "code_hash": hash }),
        YOCTO,
        COUNCIL_THRESHOLD,
    )
    .await?;
    fleet.worker.fast_forward(10).await?;

    let attempt = fleet
        .operator
        .call(fleet.registrar.id(), "upgrade")
        .args_json(json!({
            "code": base64::engine::general_purpose::STANDARD.encode(&junk),
        }))
        .max_gas()
        .transact()
        .await?;
    assert!(attempt.is_failure(), "invalid code deployed: {attempt:#?}");
    assert!(
        format!("{attempt:#?}").contains("the approval still stands"),
        "the caller was not told the deploy failed: {attempt:#?}"
    );

    let view: serde_json::Value = fleet.registrar.view("opener_view").await?.json()?;
    assert_eq!(
        view["pending_code"]["code_hash"], hash,
        "a failed deploy burned the council's approval"
    );

    let code = ours_wasm()?;
    let good = bs58::encode(Sha256::digest(&code)).into_string();
    dao_calls(
        &fleet,
        "approve_code",
        json!({ "code_hash": good }),
        YOCTO,
        COUNCIL_THRESHOLD,
    )
    .await?;
    fleet.worker.fast_forward(10).await?;
    let retry = fleet
        .operator
        .call(fleet.registrar.id(), "upgrade")
        .args_json(json!({
            "code": base64::engine::general_purpose::STANDARD.encode(&code),
        }))
        .max_gas()
        .transact()
        .await?;
    assert!(retry.is_success(), "the retry failed: {retry:#?}");
    Ok(())
}

#[tokio::test]
async fn a_stranger_cannot_drive_any_privileged_method() -> Result<()> {
    let fleet = setup().await?;
    let batch_id = draft(&fleet, &["alpha"]).await?;
    let digest = digest_of(&fleet, batch_id).await?;
    let nothing = NearToken::from_yoctonear(0);

    let attempts: Vec<(&str, serde_json::Value, NearToken)> = vec![
        (
            "approve_batch",
            json!({ "batch_id": batch_id, "digest": digest }),
            YOCTO,
        ),
        (
            "change_operator",
            json!({ "operator": fleet.stranger.id() }),
            YOCTO,
        ),
        (
            "change_admin",
            json!({ "admin": fleet.stranger.id() }),
            YOCTO,
        ),
        (
            "create_batch",
            json!({ "owner_key": fleet.owner_key, "funding": FUNDING }),
            nothing,
        ),
        (
            "create_account",
            json!({ "name": "stolen", "owner_key": fleet.owner_key }),
            FUNDING,
        ),
    ];
    for (method, args, deposit) in attempts {
        let outcome = fleet
            .stranger
            .call(fleet.registrar.id(), method)
            .args_json(args)
            .deposit(deposit)
            .max_gas()
            .transact()
            .await?;
        assert!(
            outcome.is_failure(),
            "a stranger reached {method}: {outcome:#?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_proxy_cannot_open_a_name_even_when_registrar_signs_the_call() -> Result<()> {
    let fleet = setup().await?;
    let proxy = fleet.worker.dev_deploy(&stub_wasm()?).await?;

    let outcome = fleet
        .registrar
        .as_account()
        .call(proxy.id(), "open")
        .args_json(json!({ "name": "proxied", "owner_key": fleet.owner_key }))
        .deposit(NearToken::from_near(1))
        .max_gas()
        .transact()
        .await?;

    assert!(
        fleet
            .worker
            .view_account(&"proxied".parse()?)
            .await
            .is_err(),
        "a proxy created a top level account, which the protocol should refuse"
    );
    let report = format!("{outcome:#?}");
    assert!(
        report.contains("CreateAccountOnlyByRegistrar"),
        "expected the protocol level refusal, got: {report}"
    );
    Ok(())
}
