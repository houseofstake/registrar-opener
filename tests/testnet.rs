use std::path::PathBuf;

use anyhow::{Context, Result};
use base64::Engine;
use near_workspaces::network::Testnet;
use near_workspaces::types::{Gas, NearToken, SecretKey};
use near_workspaces::{Account, AccountId, Contract, Worker};
use serde_json::json;

const RPC: &str = "https://test.rpc.fastnear.com";
const HOST_ENV: &str = "REHEARSAL_HOST";
const MEMBER_ENV: &str = "REHEARSAL_MEMBER";
const REHEARSAL_DELAY_NS: u64 = 2_000_000_000;

fn keystore_secret(account: &str) -> Result<SecretKey> {
    let path = PathBuf::from(std::env::var("HOME")?)
        .join(".near-credentials/testnet")
        .join(format!("{account}.json"));
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("no testnet credentials for {account}"))?;
    let parsed: serde_json::Value = serde_json::from_str(&raw)?;
    let key = parsed["private_key"]
        .as_str()
        .context("credential file has no private_key")?;
    key.parse().context("credential private_key does not parse")
}

fn named(variable: &str) -> Result<String> {
    std::env::var(variable).with_context(|| format!("{variable} is not set"))
}

fn fixture(name: &str) -> Result<Vec<u8>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    std::fs::read(&path).with_context(|| format!("read {}", path.display()))
}

fn ours_wasm() -> Result<Vec<u8>> {
    let out = std::process::Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .context("run cargo metadata")?;
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let dir = meta["target_directory"]
        .as_str()
        .context("cargo metadata reported no target_directory")?;
    let path = std::path::PathBuf::from(dir)
        .join("near")
        .join("registrar_opener.wasm");
    std::fs::read(&path).with_context(|| format!("{} is missing", path.display()))
}

async fn connect() -> Result<Worker<Testnet>> {
    Ok(near_workspaces::testnet().rpc_addr(RPC).await?)
}

async fn account(worker: &Worker<Testnet>, id: &str) -> Result<Account> {
    let parsed: AccountId = id.parse()?;
    Ok(Account::from_secret_key(
        parsed,
        keystore_secret(id)?,
        worker,
    ))
}

#[tokio::test]
#[ignore = "runs against live testnet, needs credentials and two funded accounts"]
async fn the_multisig_installs_the_opener_over_itself_on_live_testnet() -> Result<()> {
    let worker = connect().await?;
    let host_id = named(HOST_ENV)?;
    let member_id = named(MEMBER_ENV)?;
    let host = account(&worker, &host_id).await?;
    let member = account(&worker, &member_id).await?;

    let host_contract =
        Contract::from_secret_key(host.id().clone(), keystore_secret(&host_id)?, &worker);
    host_contract
        .as_account()
        .deploy(&fixture("registrar-mainnet.wasm")?)
        .await?
        .into_result()?;
    host_contract
        .call("new")
        .args_json(json!({
            "members": [{ "account_id": host.id() }, { "account_id": member.id() }],
            "num_confirmations": 2,
        }))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    let init = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&json!({
        "admin": member.id(),
        "operator": host.id(),
        "upgrade_delay_ns": REHEARSAL_DELAY_NS.to_string(),
    }))?);

    let request_id: u32 = host
        .call(host_contract.id(), "add_request")
        .args_json(json!({
            "request": {
                "receiver_id": host_contract.id(),
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

    member
        .call(host_contract.id(), "confirm")
        .args_json(json!({ "request_id": request_id }))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    let view: serde_json::Value = host_contract.view("opener_view").await?.json()?;
    assert_eq!(view["admin"], member.id().as_str());
    assert_eq!(view["operator"], host.id().as_str());
    assert_eq!(view["state_version"], 1);
    let balance = worker.view_account(host_contract.id()).await?.balance;
    assert!(balance > NearToken::from_near(1), "host ran dry: {balance}");
    Ok(())
}
