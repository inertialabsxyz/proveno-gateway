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
//!
//! Every successful result reports `onchain` provenance in its `_meta` under
//! `proveno/provenance`: the chain, the block, and a reference (the block hash
//! for a read, the transaction hash for a transfer). That is this server's own
//! claim about where the answer came from. The gateway binds it into the trace
//! as reported; neither this server nor the gateway proves it.

use alloy::eips::{BlockId, BlockNumberOrTag};
use alloy::network::{EthereumWallet, TransactionBuilder};
use alloy::primitives::{Address, B256, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest;
use alloy::signers::local::PrivateKeySigner;
use clap::Parser;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    MetaObject, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
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

fn tools(from: Address) -> Vec<Tool> {
    vec![
        Tool::new(
            "get_balance",
            "Balance of an address, in whole milli-ETH, rounded down.",
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
            format!(
                "Send milli-ETH from {from} to an address, as a signed transaction. \
                 {from} is the only account this server can send from, set by the \
                 credential, so funds cannot be moved towards it with this tool. \
                 It also pays the gas fee, so its balance falls by slightly more \
                 than `amount` and an exact target balance is not reachable."
            ),
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

/// A structured result carrying `onchain` provenance in its `_meta`, under the
/// key proveno reserves for it. `chain` is a CAIP-2 identifier, so a consumer
/// knows which chain to check the reference against.
fn attested(value: Value, chain_id: u64, block: u64, reference: B256) -> CallToolResult {
    let mut meta = MetaObject::new();
    meta.0.insert(
        "proveno/provenance".to_string(),
        json!({
            "type": "onchain",
            "chain": format!("eip155:{chain_id}"),
            "block": block,
            "reference": reference.to_string(),
        }),
    );
    let mut result = CallToolResult::structured(value);
    result.meta = Some(meta);
    result
}

impl Wallet {
    async fn get_balance(&self, request: &CallToolRequestParams) -> CallToolResult {
        let address = match address(request, "address") {
            Ok(address) => address,
            Err(e) => return failed(e),
        };
        let chain_id = match self.provider.get_chain_id().await {
            Ok(id) => id,
            Err(e) => return failed(format!("get_balance: chain id: {e}")),
        };
        // Pin the read to one block, so the block and hash reported are the
        // state the balance was read from.
        let block = match self
            .provider
            .get_block_by_number(BlockNumberOrTag::Latest)
            .await
        {
            Ok(Some(block)) => block,
            Ok(None) => return failed("get_balance: no latest block".to_string()),
            Err(e) => return failed(format!("get_balance: latest block: {e}")),
        };
        let (number, hash) = (block.header.number, block.header.hash);
        match self
            .provider
            .get_balance(address)
            .block_id(BlockId::hash(hash))
            .await
        {
            Ok(wei) => {
                let milli = wei / U256::from(MILLI_ETH_WEI);
                attested(
                    json!({
                        "address": address.to_string(),
                        "eth_milli": milli.saturating_to::<i64>(),
                    }),
                    chain_id,
                    number,
                    hash,
                )
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
        let chain_id = match self.provider.get_chain_id().await {
            Ok(id) => id,
            Err(e) => return failed(format!("transfer: chain id: {e}")),
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
            Ok(receipt) => {
                let Some(block) = receipt.block_number else {
                    return failed("transfer: receipt has no block number".to_string());
                };
                attested(
                    json!({
                        "tx_hash": receipt.transaction_hash.to_string(),
                        "amount": amount,
                    }),
                    chain_id,
                    block,
                    receipt.transaction_hash,
                )
            }
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
        Ok(ListToolsResult::with_all_items(tools(self.from)))
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
