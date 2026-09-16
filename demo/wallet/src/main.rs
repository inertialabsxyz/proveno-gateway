//! A wallet MCP server over stdio, for the proveno-gateway demo.
//!
//! It signs real transactions against a local Anvil chain. The private key
//! arrives as `WALLET_PRIVATE_KEY`, which only the gateway sets: it is the
//! downstream's `credential`, injected into this process's environment at spawn
//! time. It is never a command line argument and never appears in a config
//! file.
//!
//! Amounts are integers in milli-ETH so they fit the VM's integer-only value
//! model.

use alloy::network::{EthereumWallet, TransactionBuilder};
use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest;
use alloy::signers::local::PrivateKeySigner;
use clap::Parser;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};

/// One milli-ETH in wei.
const MILLI_ETH_WEI: u128 = 1_000_000_000_000_000;

/// The environment variable the gateway injects the signing key into.
const KEY_VAR: &str = "WALLET_PRIVATE_KEY";

#[derive(Parser)]
#[command(
    name = "demo-wallet",
    about = "Wallet MCP server for the proveno-gateway demo"
)]
struct Cli {
    /// JSON-RPC endpoint of the chain to sign against.
    #[arg(long, default_value = "http://127.0.0.1:8545")]
    rpc_url: String,
}

#[derive(Clone)]
struct Wallet {
    provider: std::sync::Arc<dyn Provider + Send + Sync>,
    from: Address,
}

fn object_schema(properties: Value, required: &[&str]) -> rmcp::model::JsonObject {
    json!({ "type": "object", "properties": properties, "required": required })
        .as_object()
        .expect("schema literals are objects")
        .clone()
}

fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "get_balance",
            "Balance of an address, in whole milli-ETH.",
            object_schema(
                json!({ "address": { "type": "string", "description": "0x-prefixed address." } }),
                &["address"],
            ),
        )
        .with_raw_output_schema(
            object_schema(
                json!({
                    "address": { "type": "string" },
                    "eth_milli": { "type": "integer" },
                }),
                &["address", "eth_milli"],
            )
            .into(),
        ),
        Tool::new(
            "transfer",
            "Send milli-ETH from the wallet to an address, as a signed transaction.",
            object_schema(
                json!({
                    "to": { "type": "string", "description": "0x-prefixed recipient." },
                    "amount": { "type": "integer", "description": "Milli-ETH to send." },
                }),
                &["to", "amount"],
            ),
        )
        .with_raw_output_schema(
            object_schema(
                json!({
                    "tx_hash": { "type": "string" },
                    "amount": { "type": "integer" },
                }),
                &["tx_hash", "amount"],
            )
            .into(),
        ),
    ]
}

fn arg<'a>(request: &'a CallToolRequestParams, name: &str) -> Option<&'a Value> {
    request.arguments.as_ref().and_then(|a| a.get(name))
}

fn address(request: &CallToolRequestParams, name: &str) -> Result<Address, String> {
    let text = arg(request, name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("`{name}` must be a 0x-prefixed address"))?;
    text.parse().map_err(|e| format!("`{name}`: {e}"))
}

fn failed(message: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

impl Wallet {
    async fn get_balance(&self, request: &CallToolRequestParams) -> CallToolResult {
        let address = match address(request, "address") {
            Ok(address) => address,
            Err(e) => return failed(e),
        };
        match self.provider.get_balance(address).await {
            Ok(wei) => {
                let milli = wei / U256::from(MILLI_ETH_WEI);
                CallToolResult::structured(json!({
                    "address": address.to_string(),
                    "eth_milli": milli.saturating_to::<i64>(),
                }))
            }
            Err(e) => failed(format!("get_balance: {e}")),
        }
    }

    async fn transfer(&self, request: &CallToolRequestParams) -> CallToolResult {
        let to = match address(request, "to") {
            Ok(to) => to,
            Err(e) => return failed(e),
        };
        let Some(amount) = arg(request, "amount")
            .and_then(Value::as_i64)
            .filter(|a| *a > 0)
        else {
            return failed("`amount` must be a positive whole number of milli-ETH".to_string());
        };
        let value = U256::from(amount as u128 * MILLI_ETH_WEI);
        let tx = TransactionRequest::default()
            .with_from(self.from)
            .with_to(to)
            .with_value(value);
        let pending = match self.provider.send_transaction(tx).await {
            Ok(pending) => pending,
            Err(e) => return failed(format!("transfer: {e}")),
        };
        // Wait for the receipt so the transaction is on the chain by the time
        // the program sees the hash.
        match pending.get_receipt().await {
            Ok(receipt) => CallToolResult::structured(json!({
                "tx_hash": receipt.transaction_hash.to_string(),
                "amount": amount,
            })),
            Err(e) => failed(format!("transfer: {e}")),
        }
    }
}

impl ServerHandler for Wallet {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let result = match request.name.as_ref() {
            "get_balance" => self.get_balance(&request).await,
            "transfer" => self.transfer(&request).await,
            other => {
                return Err(ErrorData::invalid_params(
                    format!("unknown tool `{other}`"),
                    None,
                ));
            }
        };
        Ok(result.into())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let key = std::env::var(KEY_VAR).map_err(|_| {
        anyhow::anyhow!(
            "{KEY_VAR} is not set; the gateway injects it as this downstream's credential"
        )
    })?;
    let signer: PrivateKeySigner = key
        .parse()
        .map_err(|e| anyhow::anyhow!("{KEY_VAR} is not a valid private key: {e}"))?;
    let from = signer.address();
    let provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer))
        .connect_http(cli.rpc_url.parse()?);

    let wallet = Wallet {
        provider: std::sync::Arc::new(provider),
        from,
    };
    let service = wallet
        .serve((tokio::io::stdin(), tokio::io::stdout()))
        .await?;
    service.waiting().await?;
    Ok(())
}
