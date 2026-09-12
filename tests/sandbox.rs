use anyhow::{Context, Result};
use near_workspaces::network::Sandbox;
use near_workspaces::types::{Gas, KeyType, NearToken, SecretKey};
use near_workspaces::{Account, Contract, Worker};
use serde_json::json;

const FUNDING: NearToken = NearToken::from_millinear(10);
const FAR_FUTURE_NS: u64 = 4_000_000_000_000_000_000;

struct Fleet {
    worker: Worker<Sandbox>,
    registrar: Contract,
    alice: Account,
    bob: Account,
    grantee: Account,
    owner_key: near_workspaces::types::PublicKey,
}

fn deployed_wasm() -> Result<Vec<u8>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("registrar-mainnet.wasm");
    std::fs::read(&path).with_context(|| format!("read {}", path.display()))
}

fn ours_wasm() -> Result<Vec<u8>> {
    let path = build_dir()?.join("near").join("registrar_opener.wasm");
    std::fs::read(&path).with_context(|| {
        format!(
            "{} is missing, run cargo near build non-reproducible-wasm --no-abi",
            path.display()
        )
    })
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

async fn install(worker: &Worker<Sandbox>, wasm: &[u8], sk: &SecretKey) -> Result<Contract> {
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

async fn setup_live_multisig() -> Result<Fleet> {
    let worker = near_workspaces::sandbox().await?;
    let sk = SecretKey::from_seed(KeyType::ED25519, "registrar-opener");
    let registrar = install(&worker, &deployed_wasm()?, &sk).await?;

    let alice = worker.dev_create_account().await?;
    let bob = worker.dev_create_account().await?;
    let grantee = worker.dev_create_account().await?;

    let outcome = registrar
        .call("new")
        .args_json(json!({
            "members": [
                { "account_id": alice.id() },
                { "account_id": bob.id() },
            ],
            "num_confirmations": 2,
        }))
        .max_gas()
        .transact()
        .await?;
    assert!(outcome.is_success(), "multisig new failed: {:#?}", outcome);

    let owner_key = SecretKey::from_seed(KeyType::ED25519, "cohort-owner").public_key();
    Ok(Fleet {
        worker,
        registrar,
        alice,
        bob,
        grantee,
        owner_key,
    })
}

async fn multisig_state(fleet: &Fleet) -> Result<(usize, u32, u32, usize)> {
    let members: Vec<serde_json::Value> = fleet.registrar.view("get_members").await?.json()?;
    let confirmations: u32 = fleet
        .registrar
        .view("get_num_confirmations")
        .await?
        .json()?;
    let nonce: u32 = fleet.registrar.view("get_request_nonce").await?.json()?;
    let requests: Vec<u32> = fleet.registrar.view("list_request_ids").await?.json()?;
    Ok((members.len(), confirmations, nonce, requests.len()))
}

async fn add_pending_request(fleet: &Fleet) -> Result<u32> {
    let outcome = fleet
        .alice
        .call(fleet.registrar.id(), "add_request")
        .args_json(json!({
            "request": {
                "receiver_id": fleet.bob.id(),
                "actions": [{ "type": "Transfer", "amount": "1" }],
            }
        }))
        .max_gas()
        .transact()
        .await?;
    assert!(outcome.is_success(), "add_request failed: {:#?}", outcome);
    Ok(outcome.json()?)
}

async fn upgrade_in_place(fleet: &Fleet) -> Result<()> {
    let outcome = fleet
        .registrar
        .as_account()
        .deploy(&ours_wasm()?)
        .await?
        .into_result()?;
    let _ = outcome;
    Ok(())
}

async fn grant(
    fleet: &Fleet,
    names: &[&str],
) -> Result<near_workspaces::result::ExecutionFinalResult> {
    Ok(fleet
        .registrar
        .call("grant_names")
        .args_json(json!({
            "grantee": fleet.grantee.id(),
            "names": names,
            "owner_key": fleet.owner_key,
            "funding": FUNDING,
            "expires_at_ns": FAR_FUTURE_NS.to_string(),
        }))
        .max_gas()
        .transact()
        .await?)
}

async fn open(
    fleet: &Fleet,
    names: &[&str],
    gas: Gas,
) -> Result<near_workspaces::result::ExecutionFinalResult> {
    Ok(fleet
        .grantee
        .call(fleet.registrar.id(), "open_names")
        .args_json(json!({ "names": names }))
        .gas(gas)
        .transact()
        .await?)
}

async fn remaining(fleet: &Fleet) -> Result<u64> {
    let g: Option<serde_json::Value> = fleet
        .registrar
        .view("get_grant")
        .args_json(json!({ "grantee": fleet.grantee.id() }))
        .await?
        .json()?;
    Ok(g.and_then(|g| g["remaining"].as_u64()).unwrap_or_default())
}

#[tokio::test]
async fn the_live_multisig_state_survives_the_upgrade_intact() -> Result<()> {
    let fleet = setup_live_multisig().await?;
    let request_id = add_pending_request(&fleet).await?;
    let before = multisig_state(&fleet).await?;
    assert_eq!(before, (2, 2, 1, 1), "baseline state is not what we expect");

    upgrade_in_place(&fleet).await?;

    let after = multisig_state(&fleet).await?;
    assert_eq!(
        before, after,
        "the multisig state did not survive deploying our build over it"
    );

    let request: serde_json::Value = fleet
        .registrar
        .view("get_request")
        .args_json(json!({ "request_id": request_id }))
        .await?
        .json()?;
    assert_eq!(
        request["receiver_id"],
        fleet.bob.id().as_str(),
        "the pending request was corrupted by the upgrade"
    );
    Ok(())
}

#[tokio::test]
async fn the_multisig_still_executes_requests_after_the_upgrade() -> Result<()> {
    let fleet = setup_live_multisig().await?;
    upgrade_in_place(&fleet).await?;

    let payee = fleet.worker.dev_create_account().await?;
    let before = fleet.worker.view_account(payee.id()).await?.balance;
    let request_id: u32 = fleet
        .alice
        .call(fleet.registrar.id(), "add_request")
        .args_json(json!({
            "request": {
                "receiver_id": payee.id(),
                "actions": [{ "type": "Transfer", "amount": NearToken::from_near(1) }],
            }
        }))
        .max_gas()
        .transact()
        .await?
        .json()?;

    for member in [&fleet.alice, &fleet.bob] {
        let confirm = member
            .call(fleet.registrar.id(), "confirm")
            .args_json(json!({ "request_id": request_id }))
            .max_gas()
            .transact()
            .await?;
        assert!(
            confirm.is_success(),
            "confirm by {} failed: {confirm:#?}",
            member.id()
        );
    }

    let after = fleet.worker.view_account(payee.id()).await?.balance;
    assert!(
        after > before,
        "the transfer the multisig approved never landed: {} -> {}",
        before,
        after
    );
    Ok(())
}

#[tokio::test]
async fn a_granted_caller_opens_real_top_level_accounts() -> Result<()> {
    let fleet = setup_live_multisig().await?;
    upgrade_in_place(&fleet).await?;
    grant(&fleet, &["alpha", "bravo"]).await?.into_result()?;

    let outcome = open(&fleet, &["alpha", "bravo"], Gas::from_tgas(300)).await?;
    assert!(outcome.is_success(), "open_names failed: {:#?}", outcome);

    for name in ["alpha", "bravo"] {
        let acct = fleet.worker.view_account(&name.parse()?).await?;
        let required = acct.storage_usage as u128 * 10u128.pow(19);
        assert!(
            acct.balance.as_yoctonear() > required,
            "{} is below its own storage floor",
            name
        );
        let keys = fleet.worker.view_access_keys(&name.parse()?).await?;
        assert_eq!(keys.len(), 1, "{name} should carry exactly the owner key");
    }
    assert_eq!(remaining(&fleet).await?, 0);
    Ok(())
}

#[tokio::test]
async fn only_the_council_can_grant() -> Result<()> {
    let fleet = setup_live_multisig().await?;
    upgrade_in_place(&fleet).await?;

    let stolen = fleet
        .alice
        .call(fleet.registrar.id(), "grant_names")
        .args_json(json!({
            "grantee": fleet.alice.id(),
            "names": ["stolen"],
            "owner_key": fleet.owner_key,
            "funding": FUNDING,
            "expires_at_ns": FAR_FUTURE_NS.to_string(),
        }))
        .max_gas()
        .transact()
        .await?;
    assert!(
        stolen.is_failure(),
        "a multisig member granted directly, bypassing the vote"
    );
    Ok(())
}

#[tokio::test]
async fn a_name_outside_the_grant_cannot_be_opened() -> Result<()> {
    let fleet = setup_live_multisig().await?;
    upgrade_in_place(&fleet).await?;
    grant(&fleet, &["alpha"]).await?.into_result()?;

    let outcome = open(&fleet, &["notgranted"], Gas::from_tgas(300)).await?;
    assert!(outcome.is_failure(), "an ungranted name was opened");
    assert!(fleet
        .worker
        .view_account(&"notgranted".parse()?)
        .await
        .is_err());
    Ok(())
}

#[tokio::test]
async fn a_revoked_grant_leaves_no_usable_approvals() -> Result<()> {
    let fleet = setup_live_multisig().await?;
    upgrade_in_place(&fleet).await?;
    grant(&fleet, &["alpha", "bravo", "charlie"])
        .await?
        .into_result()?;

    fleet
        .registrar
        .call("revoke_grant")
        .args_json(json!({ "grantee": fleet.grantee.id() }))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    grant(&fleet, &["delta"]).await?.into_result()?;

    let stale = open(&fleet, &["alpha"], Gas::from_tgas(300)).await?;
    assert!(
        stale.is_failure(),
        "a name from the revoked grant was still openable under the new grant"
    );
    assert!(fleet.worker.view_account(&"alpha".parse()?).await.is_err());

    let fresh = open(&fleet, &["delta"], Gas::from_tgas(300)).await?;
    assert!(
        fresh.is_success(),
        "the new grant does not work: {:#?}",
        fresh
    );
    Ok(())
}

#[tokio::test]
async fn a_name_that_already_exists_returns_its_slot_to_the_grant() -> Result<()> {
    let fleet = setup_live_multisig().await?;
    upgrade_in_place(&fleet).await?;
    let taken = fleet.worker.dev_create_tla().await?;
    let name = taken.id().as_str().to_string();
    grant(&fleet, &[name.as_str()]).await?.into_result()?;
    assert_eq!(remaining(&fleet).await?, 1);

    let outcome = open(&fleet, &[name.as_str()], Gas::from_tgas(300)).await?;
    assert!(outcome.is_success(), "the outer call should survive");

    assert_eq!(
        remaining(&fleet).await?,
        1,
        "a failed creation must return the slot"
    );
    let stats: serde_json::Value = fleet.registrar.view("opener_stats").await?.json()?;
    assert_eq!(stats["failed"].as_u64().unwrap(), 1);
    Ok(())
}

#[tokio::test]
async fn short_and_malformed_names_are_refused() -> Result<()> {
    let fleet = setup_live_multisig().await?;
    upgrade_in_place(&fleet).await?;

    for bad in ["ai", "alpha.near"] {
        let outcome = grant(&fleet, &[bad]).await?;
        assert!(outcome.is_failure(), "{} was accepted into a grant", bad);
    }
    let dupes = grant(&fleet, &["alpha", "alpha"]).await?;
    assert!(dupes.is_failure(), "a duplicated name was accepted");
    Ok(())
}

#[tokio::test]
async fn the_documented_per_call_maximum_fits_and_every_name_lands() -> Result<()> {
    let fleet = setup_live_multisig().await?;
    upgrade_in_place(&fleet).await?;
    let all: Vec<String> = (0..20).map(|i| format!("m{i:03}")).collect();
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    grant(&fleet, &refs).await?.into_result()?;

    let outcome = open(&fleet, &refs, Gas::from_tgas(300)).await?;
    assert!(
        outcome.is_success(),
        "the documented batch of 20 does not fit: {:#?}",
        outcome
    );
    for name in &all {
        assert!(
            fleet.worker.view_account(&name.parse()?).await.is_ok(),
            "{} did not land in a full-size batch",
            name
        );
    }
    println!(
        "MAX BATCH 20 burnt {:.1} Tgas",
        outcome.total_gas_burnt.as_gas() as f64 / 1e12
    );
    assert_eq!(remaining(&fleet).await?, 0);
    Ok(())
}

#[tokio::test]
#[ignore]
async fn probe_the_largest_batch_that_fits() -> Result<()> {
    let fleet = setup_live_multisig().await?;
    upgrade_in_place(&fleet).await?;
    let all: Vec<String> = (0..60).map(|i| format!("p{i:03}")).collect();
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    grant(&fleet, &refs).await?.into_result()?;

    let mut cursor = 0usize;
    for count in [10usize, 15, 18, 20] {
        let batch: Vec<&str> = refs.iter().skip(cursor).take(count).cloned().collect();
        cursor += count;
        let outcome = open(&fleet, &batch, Gas::from_tgas(300)).await?;
        let mut landed = 0;
        for n in &batch {
            if fleet.worker.view_account(&n.parse()?).await.is_ok() {
                landed += 1;
            }
        }
        println!(
            "PROBE count={count} outer={} burnt={:.1} Tgas landed={landed}/{count}",
            if outcome.is_success() { "ok" } else { "FAIL" },
            outcome.total_gas_burnt.as_gas() as f64 / 1e12
        );
    }
    Ok(())
}
