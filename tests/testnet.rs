use std::path::PathBuf;

use anyhow::{Context, Result};
use near_sdk::json_types::{Base64VecU8, U128, U64};
use near_workspaces::network::Testnet;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::{Gas, KeyType, NearToken, SecretKey};
use near_workspaces::{Account, AccountId, Contract, Worker};
use registrar_opener::MultiSigRequestAction;
use serde_json::json;

const RPC: &str = "https://test.rpc.fastnear.com";
const HOST: &str = "regopen-reg.testnet";
const ROOT: &str = "regopen-rehearsal.testnet";
const GRANTEE: &str = "opener.regopen-rehearsal.testnet";

const COHORT_ENV: &str = "COHORT_FILE";

const SEAT_ONE: &str = "regopen-council-seat-one";
const SEAT_TWO: &str = "regopen-council-seat-two";
const COHORT_OWNER: &str = "regopen-cohort-owner";

const FUNDING: NearToken = NearToken::from_millinear(10);
const FAR_FUTURE_NS: u64 = 4_000_000_000_000_000_000;
const GRANT_CALL_GAS: u64 = 150_000_000_000_000;

fn keystore_secret(account: &str) -> Result<SecretKey> {
    let path = PathBuf::from(std::env::var("HOME")?)
        .join(".near-credentials/testnet")
        .join(format!("{}.json", account));
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("no testnet credentials for {}", account))?;
    let parsed: serde_json::Value = serde_json::from_str(&raw)?;
    let key = parsed["private_key"]
        .as_str()
        .context("credential file has no private_key")?;
    key.parse().context("credential private_key does not parse")
}

fn seat(which: &str) -> SecretKey {
    SecretKey::from_seed(KeyType::ED25519, which)
}

async fn connect() -> Result<Worker<Testnet>> {
    Ok(near_workspaces::testnet().rpc_addr(RPC).await?)
}

fn tgas(gas: Gas) -> f64 {
    gas.as_gas() as f64 / 1_000_000_000_000.0
}

async fn storage_bytes(worker: &Worker<Testnet>, id: &AccountId) -> Result<u64> {
    Ok(worker.view_account(id).await?.storage_usage)
}

async fn free_balance(worker: &Worker<Testnet>, id: &AccountId) -> Result<NearToken> {
    let details = worker.view_account(id).await?;
    let staked = NearToken::from_yoctonear(details.storage_usage as u128 * 10u128.pow(19));
    Ok(details
        .balance
        .checked_sub(staked)
        .unwrap_or_else(|| NearToken::from_yoctonear(0)))
}

fn cohort() -> Result<Vec<String>> {
    let path = std::env::var(COHORT_ENV)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures")
                .join("cohort.example.json")
        });
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    Ok(serde_json::from_str(&raw)?)
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
            "{} is missing, run cargo near build non-reproducible-wasm --no-abi",
            path.display()
        )
    })
}

fn deployed_wasm() -> Result<Vec<u8>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("registrar-mainnet.wasm");
    std::fs::read(&path).with_context(|| format!("read {}", path.display()))
}

async fn view_json(contract: &Contract, method: &str) -> Result<serde_json::Value> {
    Ok(contract.view(method).await?.json()?)
}

async fn grant_of(contract: &Contract, grantee: &AccountId) -> Result<serde_json::Value> {
    Ok(contract
        .view("get_grant")
        .args_json(json!({ "grantee": grantee }))
        .await?
        .json()?)
}

async fn request(
    seat: &Account,
    receiver: &AccountId,
    actions: Vec<MultiSigRequestAction>,
) -> Result<ExecutionFinalResult> {
    Ok(seat
        .call(seat.id(), "add_request")
        .args_json(json!({
            "request": { "receiver_id": receiver, "actions": actions },
        }))
        .max_gas()
        .transact()
        .await?)
}

async fn confirm(seat: &Account, request_id: u32) -> Result<ExecutionFinalResult> {
    Ok(seat
        .call(seat.id(), "confirm")
        .args_json(json!({ "request_id": request_id }))
        .max_gas()
        .transact()
        .await?)
}

async fn confirm_to_execution(
    worker: &Worker<Testnet>,
    host_id: &AccountId,
    seats: &[&str],
    request_id: u32,
) -> Result<()> {
    for name in seats {
        let seat = Account::from_secret_key(host_id.clone(), seat(name), worker);
        match confirm(&seat, request_id).await?.into_result() {
            Ok(done) => println!(
                "  {} confirmed, {:.1} Tgas",
                name,
                tgas(done.total_gas_burnt)
            ),
            Err(err) if format!("{}", err).contains("Already confirmed") => {
                println!("  {} had already confirmed", name)
            }
            Err(err) if format!("{}", err).contains("No such request") => {
                println!("  request {} already executed", request_id);
                return Ok(());
            }
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}

async fn already_initialised(contract: &Contract) -> bool {
    contract.view("get_members").await.is_ok()
}

async fn pending_deploy_request(contract: &Contract) -> Result<Option<u32>> {
    let ids: Vec<u32> = contract.view("list_request_ids").await?.json()?;
    for id in ids {
        let request: serde_json::Value = contract
            .view("get_request")
            .args_json(json!({ "request_id": id }))
            .await?
            .json()?;
        if request["actions"][0]["type"] == "DeployContract" {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

#[tokio::test]
#[ignore]
async fn a_the_council_upgrades_the_multisig_through_its_own_governance() -> Result<()> {
    let worker = connect().await?;
    let host_id: AccountId = HOST.parse()?;
    let root_id: AccountId = ROOT.parse()?;

    let ours = ours_wasm()?;
    let deployed = deployed_wasm()?;
    println!(
        "mainnet multisig {} bytes, our build {} bytes",
        deployed.len(),
        ours.len()
    );

    let owner = Account::from_secret_key(host_id.clone(), keystore_secret(HOST)?, &worker);
    let contract = Contract::from_secret_key(host_id.clone(), keystore_secret(HOST)?, &worker);
    println!(
        "host free balance {}",
        free_balance(&worker, &host_id).await?
    );

    if !already_initialised(&contract).await {
        let install = owner.deploy(&deployed).await?;
        let install_gas = install.details.total_gas_burnt;
        install.into_result()?;
        println!(
            "real mainnet multisig installed, {:.1} Tgas, storage {} bytes",
            tgas(install_gas),
            storage_bytes(&worker, &host_id).await?
        );
        let init = contract
            .call("new")
            .args_json(json!({
                "members": [
                    { "public_key": seat(SEAT_ONE).public_key() },
                    { "public_key": seat(SEAT_TWO).public_key() },
                ],
                "num_confirmations": 2,
            }))
            .max_gas()
            .transact()
            .await?
            .into_result()?;
        println!(
            "initialised 2 of 2 with access key members, {:.1} Tgas",
            tgas(init.total_gas_burnt)
        );
    } else {
        println!("multisig already live on {}, reusing it", host_id);
    }

    let one = Account::from_secret_key(host_id.clone(), seat(SEAT_ONE), &worker);
    let existing: Vec<u32> = contract.view("list_request_ids").await?.json()?;
    let canary_id = match existing.first() {
        Some(id) => *id,
        None => {
            let pending = request(
                &one,
                &root_id,
                vec![MultiSigRequestAction::Transfer { amount: U128(1) }],
            )
            .await?
            .into_result()?;
            let id: u32 = pending.json()?;
            println!(
                "member key queued canary request {}, {:.1} Tgas",
                id,
                tgas(pending.total_gas_burnt)
            );
            id
        }
    };

    let upgrade_id = match pending_deploy_request(&contract).await? {
        Some(id) => {
            println!("an upgrade request is already queued as {}", id);
            id
        }
        None => {
            let queued = request(
                &one,
                &host_id,
                vec![MultiSigRequestAction::DeployContract {
                    code: Base64VecU8(ours.clone()),
                }],
            )
            .await?
            .into_result()?;
            let id: u32 = queued.json()?;
            println!(
                "upgrade queued as request {}, {:.1} Tgas",
                id,
                tgas(queued.total_gas_burnt)
            );
            id
        }
    };
    println!(
        "peak storage {} bytes, free balance {}",
        storage_bytes(&worker, &host_id).await?,
        free_balance(&worker, &host_id).await?
    );

    let members_before = view_json(&contract, "get_members").await?;
    let nonce_before = view_json(&contract, "get_request_nonce").await?;
    let confirmations_before = view_json(&contract, "get_num_confirmations").await?;
    let canary_before: serde_json::Value = contract
        .view("get_request")
        .args_json(json!({ "request_id": canary_id }))
        .await?
        .json()?;

    confirm_to_execution(&worker, &host_id, &[SEAT_ONE, SEAT_TWO], upgrade_id).await?;
    println!(
        "deploy executed, storage now {} bytes, free {}",
        storage_bytes(&worker, &host_id).await?,
        free_balance(&worker, &host_id).await?
    );

    assert_eq!(
        members_before,
        view_json(&contract, "get_members").await?,
        "the member set did not survive the governance upgrade"
    );
    assert_eq!(
        nonce_before,
        view_json(&contract, "get_request_nonce").await?,
        "the request nonce did not survive the governance upgrade"
    );
    assert_eq!(
        confirmations_before,
        view_json(&contract, "get_num_confirmations").await?,
        "the threshold did not survive the governance upgrade"
    );
    let canary_after: serde_json::Value = contract
        .view("get_request")
        .args_json(json!({ "request_id": canary_id }))
        .await?
        .json()?;
    assert_eq!(
        canary_before, canary_after,
        "the pending request was corrupted by the upgrade"
    );

    let stats = view_json(&contract, "opener_stats").await?;
    println!("opener methods answer after the upgrade, stats {}", stats);
    Ok(())
}

#[tokio::test]
#[ignore]
async fn b_the_council_grants_the_whole_cohort_in_one_request() -> Result<()> {
    let worker = connect().await?;
    let host_id: AccountId = HOST.parse()?;
    let grantee_id: AccountId = GRANTEE.parse()?;
    let names = cohort()?;
    let contract = Contract::from_secret_key(host_id.clone(), keystore_secret(HOST)?, &worker);
    let one = Account::from_secret_key(host_id.clone(), seat(SEAT_ONE), &worker);

    let args = json!({
        "grantee": grantee_id,
        "names": names,
        "owner_key": seat(COHORT_OWNER).public_key(),
        "funding": FUNDING,
        "expires_at_ns": FAR_FUTURE_NS.to_string(),
    })
    .to_string();
    println!(
        "{} names, grant_names args {} bytes",
        names.len(),
        args.len()
    );

    let queued = request(
        &one,
        &host_id,
        vec![MultiSigRequestAction::FunctionCall {
            method_name: "grant_names".to_string(),
            args: Base64VecU8(args.into_bytes()),
            deposit: U128(0),
            gas: U64(GRANT_CALL_GAS),
        }],
    )
    .await?
    .into_result()?;
    let grant_id: u32 = queued.json()?;
    println!(
        "grant queued as request {}, {:.1} Tgas",
        grant_id,
        tgas(queued.total_gas_burnt)
    );

    let before = storage_bytes(&worker, &host_id).await?;
    confirm_to_execution(&worker, &host_id, &[SEAT_ONE, SEAT_TWO], grant_id).await?;
    let after = storage_bytes(&worker, &host_id).await?;
    println!(
        "grant executed, storage {} to {} bytes, {} bytes for {} names",
        before,
        after,
        after.saturating_sub(before),
        names.len()
    );

    let grant = grant_of(&contract, &grantee_id).await?;
    assert_eq!(grant["issued"], names.len() as u64);
    assert_eq!(grant["remaining"], names.len() as u64);
    println!("grant on chain {}", grant);
    Ok(())
}

#[tokio::test]
#[ignore]
async fn c_a_creation_the_protocol_refuses_returns_every_slot() -> Result<()> {
    let worker = connect().await?;
    let host_id: AccountId = HOST.parse()?;
    let grantee_id: AccountId = GRANTEE.parse()?;
    let contract = Contract::from_secret_key(host_id.clone(), keystore_secret(HOST)?, &worker);
    let grantee = Account::from_secret_key(grantee_id.clone(), keystore_secret(GRANTEE)?, &worker);

    let names = cohort()?;
    let batch: Vec<&str> = names.iter().take(2).map(|n| n.as_str()).collect();

    let before = grant_of(&contract, &grantee_id).await?;
    let stats_before = view_json(&contract, "opener_stats").await?;

    let outcome = grantee
        .call(&host_id, "open_names")
        .args_json(json!({ "names": batch }))
        .max_gas()
        .transact()
        .await?
        .into_result()?;
    let taken: u32 = outcome.json()?;
    println!(
        "open_names took {} slots, {:.1} Tgas",
        taken,
        tgas(outcome.total_gas_burnt)
    );
    for failure in outcome.receipt_failures() {
        println!("receipt failure: {:?}", failure);
    }

    let after = grant_of(&contract, &grantee_id).await?;
    let stats_after = view_json(&contract, "opener_stats").await?;
    println!(
        "remaining {} to {}, stats {} to {}",
        before["remaining"], after["remaining"], stats_before, stats_after
    );
    assert_eq!(
        before["remaining"], after["remaining"],
        "a creation the protocol refused must return every slot to the grant"
    );
    assert_eq!(
        stats_after["failed"].as_u64().unwrap_or_default(),
        stats_before["failed"].as_u64().unwrap_or_default() + batch.len() as u64,
        "each refused creation must be counted"
    );
    Ok(())
}
