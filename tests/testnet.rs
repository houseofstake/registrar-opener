use anyhow::{Context, Result};
use base64::Engine;
use near_workspaces::network::Testnet;
use near_workspaces::types::{Gas, NearToken};
use near_workspaces::{Contract, Worker};
use serde_json::json;

const RPC: &str = "https://test.rpc.fastnear.com";

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
    std::fs::read(&path).with_context(|| {
        format!(
            "{} is missing, run cargo near build non-reproducible-wasm --locked --no-abi",
            path.display()
        )
    })
}

async fn connect() -> Result<Worker<Testnet>> {
    Ok(near_workspaces::testnet().rpc_addr(RPC).await?)
}

#[tokio::test]
#[ignore = "runs against live testnet and spends faucet funds"]
async fn the_multisig_installs_the_opener_over_itself_on_live_testnet() -> Result<()> {
    let worker = connect().await?;

    let host_account = worker.dev_create_account().await?;
    let host = Contract::from_secret_key(
        host_account.id().clone(),
        host_account.secret_key().clone(),
        &worker,
    );

    let alice = worker.dev_create_account().await?;
    let bob = worker.dev_create_account().await?;
    println!("rehearsal host {}", host.id());

    host.as_account()
        .deploy(&fixture("registrar-mainnet.wasm")?)
        .await?
        .into_result()?;
    host.call("new")
        .args_json(json!({
            "members": [{ "account_id": alice.id() }, { "account_id": bob.id() }],
            "num_confirmations": 2,
        }))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    let members: Vec<serde_json::Value> = host.view("get_members").await?.json()?;
    assert_eq!(members.len(), 2, "the multisig did not come up on testnet");
    let threshold: u32 = host.view("get_num_confirmations").await?.json()?;
    assert_eq!(threshold, 2);

    let council = worker.dev_create_account().await?;
    let operator = worker.dev_create_account().await?;
    let code = ours_wasm()?;
    let init = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&json!({
        "admin": council.id(),
        "operator": operator.id(),
    }))?);

    let request_id: u32 = alice
        .call(host.id(), "add_request_and_confirm")
        .args_json(json!({
            "request": {
                "receiver_id": host.id(),
                "actions": [
                    {
                        "type": "DeployContract",
                        "code": base64::engine::general_purpose::STANDARD.encode(&code),
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

    let confirm = bob
        .call(host.id(), "confirm")
        .args_json(json!({ "request_id": request_id }))
        .max_gas()
        .transact()
        .await?;
    assert!(confirm.is_success(), "confirm failed: {confirm:#?}");

    let view: serde_json::Value = host.view("opener_view").await?.json()?;
    assert_eq!(view["admin"], council.id().as_str());
    assert_eq!(view["operator"], operator.id().as_str());
    assert_eq!(view["state_version"], 1);
    assert!(
        host.view("get_members").await.is_err(),
        "the multisig methods survived the replacement"
    );

    let details = worker.view_account(host.id()).await?;
    let floor = NearToken::from_yoctonear(details.storage_usage as u128 * 10u128.pow(19));
    assert!(
        details.balance > floor,
        "host is below its storage floor: {} vs {floor}",
        details.balance
    );
    println!(
        "installed on {} at {} bytes, balance {}",
        host.id(),
        details.storage_usage,
        details.balance
    );
    Ok(())
}
