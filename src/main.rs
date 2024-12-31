use {
    backoff::{future::retry, ExponentialBackoff},
    clap::Parser as ClapParser,
    futures::{future::TryFutureExt, sink::SinkExt, stream::StreamExt},
    log::{error, info},
    std::{collections::HashMap, env, sync::Arc, time::Duration},
    tokio::sync::Mutex,
    tonic::transport::channel::ClientTlsConfig,
    yellowstone_grpc_client::{GeyserGrpcClient, Interceptor},
    yellowstone_grpc_proto::prelude::{
        subscribe_update::UpdateOneof, CommitmentLevel, SubscribeRequest,
        SubscribeRequestFilterAccounts, SubscribeRequestPing,
        SubscribeRequestFilterTransactions
    },
    yellowstone_vixen_core::Parser as VixenParser,
    yellowstone_vixen_parser::raydium::{AccountParser as RaydiumParser, RaydiumProgramState},
};

type AccountFilterMap = HashMap<String, SubscribeRequestFilterAccounts>;
type TransactionFilterMap = HashMap<String, SubscribeRequestFilterTransactions>;
#[derive(Debug, Clone, ClapParser)]
#[clap(author, version, about)]
struct Args {
    #[clap(short, long, help = "gRPC endpoint")]
    /// Service endpoint
    endpoint: String,

    #[clap(long, help = "X-Token")]
    x_token: String,

    #[clap(long, help = "Pool address of the raydium pool to subscribe to")]
    pool_address: String,
}



impl Args {
    async fn connect(&self) -> anyhow::Result<GeyserGrpcClient<impl Interceptor>> {
        GeyserGrpcClient::build_from_shared(self.endpoint.clone())?
            .x_token(Some(self.x_token.clone()))?
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(10))
            .tls_config(ClientTlsConfig::new().with_native_roots())?
            .max_decoding_message_size(1024 * 1024 * 1024)
            .connect()
            .await
            .map_err(Into::into)
    }

    pub fn get_raydium_subscribe_request(&self) -> anyhow::Result<SubscribeRequest> {
        let mut accounts: AccountFilterMap = HashMap::new();
        let mut transactions: TransactionFilterMap = HashMap::new();
        // 监控所有与 Raydium 程序相关的交易
        accounts.insert(
            "raydium_program".to_owned(),
            SubscribeRequestFilterAccounts {
                account: vec![],  // 不限制具体账户
                owner: vec!["675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string()], // Raydium 程序 ID
                filters: vec![],
                nonempty_txn_signature: Some(true), // 只获取有签名的交易
            }
        );

        transactions.insert(
            "raydium_program".to_owned(),
            SubscribeRequestFilterTransactions {
                vote: Some(false),
                failed: Some(false),
                signature: None,
                account_include: vec!["675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string()],
                account_exclude: vec![],
                account_required: vec![],
            }
        );

        Ok(SubscribeRequest {
            slots: HashMap::default(),
            accounts,
            transactions,
            transactions_status: HashMap::default(),
            entry: HashMap::default(),
            blocks: HashMap::default(),
            blocks_meta: HashMap::default(),
            commitment: Some(CommitmentLevel::Confirmed as i32),
            accounts_data_slice: Vec::default(),
            ping: None,
            from_slot:Some(1),
        })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env::set_var(
        env_logger::DEFAULT_FILTER_ENV,
        env::var_os(env_logger::DEFAULT_FILTER_ENV).unwrap_or_else(|| "info".into()),
    );
    env_logger::init();

    let args = Args::parse();
    let zero_attempts = Arc::new(Mutex::new(true));

    // The default exponential backoff strategy intervals:
    // [500ms, 750ms, 1.125s, 1.6875s, 2.53125s, 3.796875s, 5.6953125s,
    // 8.5s, 12.8s, 19.2s, 28.8s, 43.2s, 64.8s, 97s, ... ]
    retry(ExponentialBackoff::default(), move || {
        let args = args.clone();
        println!("args: {:?}", args);
        let zero_attempts = Arc::clone(&zero_attempts);

        async move {
            let mut zero_attempts = zero_attempts.lock().await;
            if *zero_attempts {
                *zero_attempts = false;
            } else {
                info!("Retry to connect to the server");
            }
            drop(zero_attempts);

            let client = args.connect().await.map_err(backoff::Error::transient)?;
            info!("Connected");

            let request = args
                .get_raydium_subscribe_request()
                .map_err(backoff::Error::Permanent)?;

            geyser_subscribe(client, request)
                .await
                .map_err(backoff::Error::transient)?;

            Ok::<(), backoff::Error<anyhow::Error>>(())
        }
        .inspect_err(|error| error!("failed to connect: {error}"))
    })
    .await
    .map_err(Into::into)
}

async fn geyser_subscribe(
    mut client: GeyserGrpcClient<impl Interceptor>,
    request: SubscribeRequest,
) -> anyhow::Result<()> {
    let (mut subscribe_tx, mut stream) = client.subscribe_with_request(Some(request)).await?;

    info!("stream opened");
    while let Some(message) = stream.next().await {
        match message {
            Ok(msg) => {
                match msg.update_oneof {
                    Some(UpdateOneof::Account(account_msg)) => {
                        if let Some(account_info) = &account_msg.account {
                            if let Some(txn_signature) = &account_info.txn_signature {
                                let txn_signature_str = bs58::encode(txn_signature).into_string();
                                let local_time = chrono::Local::now().format("%Y-%m-%d-%H:%M:%S").to_string();
                                let url = format!("https://solscan.io/tx/{}?-/{}", txn_signature_str, local_time);
                                println!("Transaction signature: {:?}", url);
                            }
                        }
                    }
                 
                    Some(UpdateOneof::Transaction(transaction_msg)) => {
                        
                        if let Some(transaction_info) = &transaction_msg.transaction {
                            let signature = transaction_info.signature.clone();
                            let signature_str = bs58::encode(signature).into_string();
    
                            // 构建 Solscan 交易链接
                            let local_time = chrono::Local::now().format("%Y-%m-%d-%H:%M:%S").to_string();
                            let url = format!("https://solscan.io/tx/{}?-/{}", signature_str, local_time);
                            println!("Transaction URL: {}", url);
                            
                            // println!("Transaction info: {:?}", transaction_info);
                            // 遍历输出所有
                            // if let Some(transaction) = &transaction_info.transaction{
                            //     println!("Transaction : {:?}", transaction);
                            //     if let Some(message) = &transaction.message{
                            //         println!("Message : {:?}", message);
                            //         // 输出消息头部信息
                            //         if let Some(header) = &message.header {
                            //             println!("Header:");
                            //             println!("  Required signatures: {}", header.num_required_signatures);
                            //             println!("  Readonly signed accounts: {}", header.num_readonly_signed_accounts);
                            //             println!("  Readonly unsigned accounts: {}", header.num_readonly_unsigned_accounts);
                            //         }

                            //         // // 输出账密钥
                            //         println!("Account Keys:");
                            //         for (i, key) in message.account_keys.iter().enumerate() {
                            //             println!("  {}: {}", i, bs58::encode(key).into_string());
                            //         }

                            //         // 输出最近的区块哈希
                            //         println!("Recent Blockhash: {}", bs58::encode(&message.recent_blockhash).into_string());

                            //         println!("Instructions: {:?}", message.instructions);
                            //         for (i, instruction) in message.instructions.iter().enumerate() {
                            //             println!("  Instruction {}:", i);
                            //             println!("    Program ID Index: {}", instruction.program_id_index);
                            //             println!("    Accounts: {:?}", instruction.accounts);
                            //             println!("    Data: {}", bs58::encode(&instruction.data).into_string());
                            //         }

                            //         if !message.address_table_lookups.is_empty() {
                            //             println!("Address Table Lookups:");
                            //             for (i, lookup) in message.address_table_lookups.iter().enumerate() {
                            //                 println!("  Lookup {}:", i);
                            //                 println!("    Account Key: {}", bs58::encode(&lookup.account_key).into_string());
                            //                 println!("    Writable Indexes: {:?}", lookup.writable_indexes);
                            //                 println!("    Readonly Indexes: {:?}", lookup.readonly_indexes);
                            //             }
                            //         }
                            //     }
                            // }
                            // 有这个属性
                            // if let Some(meta) = &transaction_info.meta {
                            //     println!("Meta : {:?}", meta);
                            //     if let Some(err) = &meta.err {
                            //         println!("Transaction Error: {:?}", err);
                            //     }

                            //     println!("Fee: {}", meta.fee);

                            //     println!("Pre balances: {:?}", meta.pre_balances);
                            //     println!("Post balances: {:?}", meta.post_balances);
                            //     println!("log_messages: {:?}", meta.log_messages);
                            //     println!("log_messages_none: {:?}", meta.log_messages_none);
                            //     println!("pre_token_balances: {:?}", meta.pre_token_balances);
                            //     println!("post_token_balances: {:?}", meta.post_token_balances);
                            //     println!("inner_instructions: {:?}", meta.inner_instructions);
                            //     println!("rewards: {:?}", meta.rewards);
                            //     println!("loaded_writable_addresses: {:?}", meta.loaded_writable_addresses);
                            //     println!("loaded_readonly_addresses: {:?}", meta.loaded_readonly_addresses);
                            //     println!("return_data: {:?}", meta.return_data);
                            //     // 输出代币余额变化

                            //     // 输出日志消息

                            //     // 输出内部指令

                            //     // 输出计算单元消耗

                            //     // 输出奖励信息

                            //     // 输出已加载的可写地址

                            //     // 输出已加载的只读地址
                            // }
                        }
                    }
                   
                    Some(UpdateOneof::Ping(_)) => {
                        subscribe_tx
                            .send(SubscribeRequest {
                                ping: Some(SubscribeRequestPing { id: 1 }),
                                ..Default::default()
                            })
                            .await?;
                    }
                    Some(UpdateOneof::Pong(_)) => {}
                    None => {
                        error!("update not found in the message");
                        break;
                    }
                    _ => {}
                }
            }
            Err(error) => {
                error!("error: {error:?}");
                break;
            }
        }
    }
    info!("stream closed");
    Ok(())
}

/**
 * Convert sqrt_price_x64 to normal price
 *
 * This does not consider mint decimals. Nor this is the right way to calculate price.
 * Use only for SOL/USD
 */
fn get_raydium_sol_usd_price(sqrt_price_x64: u128) -> f64 {
    let sqrt_price = sqrt_price_x64 as f64 / (1u128 << 64) as f64;
    sqrt_price * sqrt_price * 1000.0
}