use miden_assembly::{ast::{Module, ModuleKind}, Assembler, DefaultSourceManager, Library, LibraryPath};
use rand::{rngs::StdRng, RngCore};
use std::{fs, path::Path, sync::Arc};
use tokio::time::{sleep, Duration};
use serde::de::value::Error;

use miden_client::{
    account::{
        component::{BasicFungibleFaucet, BasicWallet, RpoFalcon512}, Account, AccountBuilder, AccountId, AccountStorageMode, AccountType, StorageSlot
    }, asset::{FungibleAsset, TokenSymbol}, auth::AuthSecretKey, builder::ClientBuilder, crypto::{FeltRng, SecretKey}, keystore::FilesystemKeyStore, note::{
        Note, NoteAssets, NoteExecutionHint, NoteExecutionMode, NoteInputs, NoteMetadata, NoteRecipient, NoteRelevance, NoteScript, NoteTag, NoteType
    }, rpc::{Endpoint, TonicRpcClient}, store::InputNoteRecord, transaction::{OutputNote, TransactionKernel, TransactionRequestBuilder, TransactionScript}, Client, ClientError, Felt, Word
};
use miden_objects::{account::{AccountComponent, NetworkId}, Hasher};

pub fn create_tx_script(
    script_code: String,
    library: Option<Library>,
) -> Result<TransactionScript, Error> {
    let assembler = TransactionKernel::assembler();

    let assembler = match library {
        Some(lib) => assembler.with_library(lib),
        None => Ok(assembler.with_debug_mode(false)),
    }
    .unwrap();
    let tx_script = TransactionScript::compile(script_code, [], assembler).unwrap();

    Ok(tx_script)
}

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

// Helper to create a basic account
async fn create_basic_account(
    client: &mut Client,
    keystore: FilesystemKeyStore<StdRng>,
) -> Result<miden_client::account::Account, ClientError> {
    let mut init_seed = [0_u8; 32];
    client.rng().fill_bytes(&mut init_seed);

    let key_pair = SecretKey::with_rng(client.rng());
    let anchor_block = client.get_latest_epoch_block().await.unwrap();
    let builder = AccountBuilder::new(init_seed)
        .anchor((&anchor_block).try_into().unwrap())
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Private)
        .with_component(RpoFalcon512::new(key_pair.public_key()))
        .with_component(BasicWallet);
    let (account, seed) = builder.build().unwrap();
    client.add_account(&account, Some(seed), false).await?;
    keystore
        .add_key(&AuthSecretKey::RpoFalcon512(key_pair))
        .unwrap();

    Ok(account)
}

async fn create_basic_faucet(
    client: &mut Client,
    keystore: FilesystemKeyStore<StdRng>,
) -> Result<miden_client::account::Account, ClientError> {
    let mut init_seed = [0u8; 32];
    client.rng().fill_bytes(&mut init_seed);
    let key_pair = SecretKey::with_rng(client.rng());
    let anchor_block = client.get_latest_epoch_block().await.unwrap();
    let symbol = TokenSymbol::new("MID").unwrap();
    let decimals = 8;
    let max_supply = Felt::new(1_000_000);
    let builder = AccountBuilder::new(init_seed)
        .anchor((&anchor_block).try_into().unwrap())
        .account_type(AccountType::FungibleFaucet)
        .storage_mode(AccountStorageMode::Public)
        .with_component(RpoFalcon512::new(key_pair.public_key()))
        .with_component(BasicFungibleFaucet::new(symbol, decimals, max_supply).unwrap());
    let (account, seed) = builder.build().unwrap();
    client.add_account(&account, Some(seed), false).await?;
    keystore
        .add_key(&AuthSecretKey::RpoFalcon512(key_pair))
        .unwrap();
    Ok(account)
}

// Helper to wait until an account has the expected number of consumable notes
pub async fn wait_for_note(
    client: &mut Client,
    account_id: &Account,
    expected: &Note,
) -> Result<(), ClientError> {
    loop {
        client.sync_state().await?;

        let notes: Vec<(InputNoteRecord, Vec<(AccountId, NoteRelevance)>)> =
            client.get_consumable_notes(Some(account_id.id())).await?;

        let found = notes.iter().any(|(rec, _)| rec.id() == expected.id());

        if found {
            println!("✅ note found {}", expected.id().to_hex());
            break;
        }

        println!("Note {} not found. Waiting...", expected.id().to_hex());
        sleep(Duration::from_secs(3)).await;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), ClientError> {

    let endpoint = Endpoint::testnet();
    let timeout_ms = 10_000;
    let rpc_api = Arc::new(TonicRpcClient::new(&endpoint, timeout_ms));

    let mut client = ClientBuilder::new()
        .with_rpc(rpc_api)
        .with_filesystem_keystore("./keystore")
        .in_debug_mode(false)
        .build()
        .await?;

    let sync_summary = client.sync_state().await.unwrap();
    println!("Latest block: {}", sync_summary.block_num);

    let keystore = FilesystemKeyStore::new("./keystore".into()).unwrap();

    // Creating new accounts and faucet
    println!("\n[STEP 1] Creating new accounts");
    let alice_account = create_basic_account(&mut client, keystore.clone()).await?;
    println!("Alice's account ID: {:?}", alice_account.id().to_bech32(NetworkId::Testnet));

    let amm_path = Path::new("./masm/amm-account.masm");
    let amm_code = fs::read_to_string(amm_path).unwrap();
    let assembler: Assembler = TransactionKernel::assembler().with_debug_mode(false);

    let amm_component = AccountComponent::compile(
        amm_code.clone(),
        assembler,
        vec![],
    )
    .unwrap()
    .with_supports_all_types();

    let mut seed = [0_u8; 32];
    client.rng().fill_bytes(&mut seed);
    let anchor_block = client.get_latest_epoch_block().await.unwrap();

    let (amm_contract, amm_seed) = AccountBuilder::new(seed)
        .anchor((&anchor_block).try_into().unwrap())
        .account_type(AccountType::RegularAccountImmutableCode)
        .storage_mode(AccountStorageMode::Private)
        .with_component(amm_component.clone())
        .with_component(RpoFalcon512::new(SecretKey::with_rng(client.rng()).public_key()))
        .build()
        .unwrap();

    println!(
        "amm_contract commitment: {:?}",
        amm_contract.commitment()
    );
    println!(
        "amm_contract id: {:?}",
        amm_contract.id().to_bech32(NetworkId::Testnet)
    );
    println!("amm_contract storage: {:?}", amm_contract.storage());

    client
        .add_account(&amm_contract.clone(), Some(amm_seed), false)
        .await
        .unwrap();

    println!("\nDeploying first fungible faucet.");
    let faucet1: Account = create_basic_faucet(&mut client, keystore.clone()).await?;
    println!("alice account storage {:?}", alice_account.storage());
    println!("Faucet account ID: {:?}", faucet1.id().to_bech32(NetworkId::Testnet));
    client.sync_state().await?;

    println!("\nDeploying second fungible faucet.");
    let faucet2 = create_basic_faucet(&mut client, keystore).await?;
    println!("Faucet account ID: {:?}", faucet2.id().to_bech32(NetworkId::Testnet));
    client.sync_state().await?;

    // Step 2: Mint tokens with P2ID for Alice
    println!("\n[STEP 2] Mint tokens with P2ID");
    let faucet_id = faucet1.id();
    let amount: u64 = 100;
    let mint_amount = FungibleAsset::new(faucet_id, amount).unwrap();
    let tx_request = TransactionRequestBuilder::new()
        .build_mint_fungible_asset(
            mint_amount,
            alice_account.id(),
            NoteType::Public,
            client.rng(),
        )
        .unwrap();
    let tx_exec = client.new_transaction(faucet1.id(), tx_request).await?;
    client.submit_transaction(tx_exec.clone()).await?;

    let p2id_note = if let OutputNote::Full(note) = tx_exec.created_notes().get_note(0) {
        note.clone()
    } else {
        panic!("Expected OutputNote::Full");
    };

    wait_for_note(&mut client, &alice_account, &p2id_note).await?;

    // Alice consume the P2ID note
    let consume_request = TransactionRequestBuilder::new()
        .with_authenticated_input_notes([(p2id_note.id(), None)])
        .build()
        .unwrap();
    let tx_exec = client
        .new_transaction(alice_account.id(), consume_request)
        .await?;
    client.submit_transaction(tx_exec).await?;
    client.sync_state().await?;


    let faucet_id2 = faucet2.id();
    let mint_amount2 = FungibleAsset::new(faucet_id2, amount).unwrap();
    let tx_request = TransactionRequestBuilder::new()
        .build_mint_fungible_asset(
            mint_amount2,
            alice_account.id(),
            NoteType::Public,
            client.rng(),
        )
        .unwrap();
    let tx_exec = client.new_transaction(faucet2.id(), tx_request).await?;
    client.submit_transaction(tx_exec.clone()).await?;

    let p2id_note2 = if let OutputNote::Full(note) = tx_exec.created_notes().get_note(0) {
        note.clone()
    } else {
        panic!("Expected OutputNote::Full");
    };

    wait_for_note(&mut client, &alice_account, &p2id_note2).await?;

    // Alice consume the P2ID note
    let consume_request = TransactionRequestBuilder::new()
        .with_authenticated_input_notes([(p2id_note2.id(), None)])
        .build()
        .unwrap();
    let tx_exec = client
        .new_transaction(alice_account.id(), consume_request)
        .await?;
    client.submit_transaction(tx_exec).await?;
    client.sync_state().await?;

    println!("\n[STEP 3] Create add liquidity note");
    let assembler = TransactionKernel::assembler().with_debug_mode(true);

    let account_component_lib = create_library(
        assembler.clone(),
        "external_contract::amm_account",
        &amm_code,
    )
    .unwrap();

    let code = fs::read_to_string(Path::new("./masm/liquidity.masm")).unwrap();
    let serial_num = client.rng().draw_word();
    let note_script = NoteScript::compile(code, assembler.with_library(account_component_lib).unwrap()).unwrap();
    let note_inputs = NoteInputs::new(vec![]).unwrap();
    let recipient = NoteRecipient::new(serial_num, note_script, note_inputs);
    let tag = NoteTag::for_local_use_case(0, 0).unwrap();
    let metadata = NoteMetadata::new(
        alice_account.id(),
        NoteType::Private,
        tag,
        NoteExecutionHint::always(),
        Felt::new(0),
    )?;
    let vault = NoteAssets::new(vec![mint_amount.into(), mint_amount2.into()])?;
    let custom_note = Note::new(vault, metadata, recipient);
    println!("note hash: {:?}", custom_note.id().to_hex());

    let note_request = TransactionRequestBuilder::new()
        .with_own_output_notes(vec![OutputNote::Full(custom_note.clone())])
        .build()
        .unwrap();
    let tx_result = client
        .new_transaction(alice_account.id(), note_request)
        .await
        .unwrap();
    println!(
        "View transaction on MidenScan: https://testnet.midenscan.com/tx/{:?}",
        tx_result.executed_transaction().id()
    );
    let _ = client.submit_transaction(tx_result).await;
    client.sync_state().await?;


    println!("\n[STEP 3] Accept liquidity note - private AMM");
    let script_code = fs::read_to_string(Path::new("./masm/nop_script.masm")).unwrap();
    let tx_script = create_tx_script(script_code, None).unwrap();

    let consume_custom_request = TransactionRequestBuilder::new()
        .with_unauthenticated_input_notes([(custom_note, None)])
        .with_custom_script(tx_script)
        .build()
        .unwrap();
    let tx_result = client
        .new_transaction(amm_contract.id(), consume_custom_request)
        .await
        .unwrap();
    println!(
        "Consumed Note Tx on MidenScan: https://testnet.midenscan.com/tx/{:?} \n",
        tx_result.executed_transaction().id()
    );
    println!("account delta: {:?}", tx_result.account_delta().vault());
    let _ = client.submit_transaction(tx_result).await;

    Ok(())
}