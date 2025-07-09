use rand::RngCore;
use std::{fs, path::Path, sync::Arc};

use miden_assembly::{
    ast::{Module, ModuleKind},
    LibraryPath,
};
use miden_client::{
    account::{AccountBuilder, AccountStorageMode, AccountType, StorageSlot},
    builder::ClientBuilder,
    rpc::{Endpoint, TonicRpcClient},
    transaction::{TransactionKernel, TransactionRequestBuilder, TransactionScript},
    ClientError, Felt,
};
use miden_objects::{
    account::{AccountComponent, NetworkId}, assembly::Assembler, assembly::DefaultSourceManager,
};

fn create_library(
    assembler: Assembler,
    library_path: &str,
    source_code: &str,
) -> Result<miden_assembly::Library, Box<dyn std::error::Error>> {
    let source_manager = Arc::new(DefaultSourceManager::default());
    let module = Module::parser(ModuleKind::Library).parse_str(
        LibraryPath::new(library_path)?,
        source_code,
        &source_manager,
    )?;
    let library = assembler.clone().assemble_library([module])?;
    Ok(library)
}

#[tokio::main]
async fn main() -> Result<(), ClientError> {
    // Initialize client
    let endpoint = Endpoint::testnet();
    let timeout_ms = 10_000;
    let rpc_api = Arc::new(TonicRpcClient::new(&endpoint, timeout_ms));

    let mut client = ClientBuilder::new()
        .with_rpc(rpc_api)
        .with_filesystem_keystore("./keystore")
        .in_debug_mode(true)
        .build()
        .await?;

    
    println!("\n[STEP 1] Creating voting contract.");

    let voting_path = Path::new("./masm/accounts/voting.masm");
    let voting_code = fs::read_to_string(voting_path).unwrap();
    let assembler: Assembler = TransactionKernel::assembler().with_debug_mode(true);

    let voting_component = AccountComponent::compile(
        voting_code.clone(),
        assembler,
        vec![StorageSlot::Value([
            Felt::new(0),
            Felt::new(0),
            Felt::new(0),
            Felt::new(0),
        ]),
        StorageSlot::Value([
            Felt::new(0),
            Felt::new(0),
            Felt::new(0),
            Felt::new(0),
        ]),
        StorageSlot::Value([
            Felt::new(0),
            Felt::new(0),
            Felt::new(0),
            Felt::new(0),
        ])],
    )
    .unwrap()
    .with_supports_all_types();

    let mut seed = [0_u8; 32];
    client.rng().fill_bytes(&mut seed);
    let anchor_block = client.get_latest_epoch_block().await.unwrap();

    let (voter_contract, counter_seed) = AccountBuilder::new(seed)
        .anchor((&anchor_block).try_into().unwrap())
        .account_type(AccountType::RegularAccountImmutableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_component(voting_component.clone())
        .build()
        .unwrap();

    println!(
        "voter_contract commitment: {:?}",
        voter_contract.commitment()
    );
    println!(
        "voter_contract id: {:?}",
        voter_contract.id().to_bech32(NetworkId::Testnet)
    );
    println!("voter_contract storage: {:?}", voter_contract.storage());

    client
        .add_account(&voter_contract.clone(), Some(counter_seed), false)
        .await
        .unwrap();

    println!("\n[STEP 2] Call voting Contract With Script");

    let script_path = Path::new("./masm/scripts/voting_script.masm");
    let script_code = fs::read_to_string(script_path).unwrap();

    let assembler: Assembler = TransactionKernel::assembler().with_debug_mode(true);
    let account_component_lib = create_library(
        assembler.clone(),
        "external_contract::voting_contract",
        &voting_code,
    )
    .unwrap();

    let tx_script = TransactionScript::compile(
        script_code,
        [],
        assembler.with_library(&account_component_lib).unwrap(),
    )
    .unwrap();

    let tx_increment_request = TransactionRequestBuilder::new()
        .with_custom_script(tx_script)
        .build()
        .unwrap();

    let tx_result = client
        .new_transaction(voter_contract.id(), tx_increment_request)
        .await
        .unwrap();

    let tx_id = tx_result.executed_transaction().id();
    println!(
        "View transaction on MidenScan: https://testnet.midenscan.com/tx/{:?}",
        tx_id
    );

    let _ = client.submit_transaction(tx_result).await;

    client.sync_state().await.unwrap();


    Ok(())
}