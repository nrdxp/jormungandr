use crate::common::jormungandr::JormungandrProcess;
use crate::common::{jormungandr::ConfigurationBuilder, startup};
use crate::jormungandr::rest::v1::assert_in_block;
use crate::jormungandr::rest::v1::assert_not_in_block;
use chain_impl_mockchain::fragment::Fragment;
use jormungandr_testing_utils::testing::FragmentSenderSetup;
use jormungandr_testing_utils::testing::FragmentVerifier;
use jormungandr_testing_utils::testing::MemPoolCheck;
use rstest::*;
use std::time::Duration;

#[fixture]
fn world() -> (JormungandrProcess, Fragment, Fragment, Fragment, Fragment) {
    let mut alice = startup::create_new_account_address();
    let mut bob = startup::create_new_account_address();
    let mut clarice = startup::create_new_account_address();
    let mut david = startup::create_new_account_address();

    let (jormungandr, _stake_pools) = startup::start_stake_pool(
        &[alice.clone()],
        &[bob.clone(), clarice.clone()],
        &mut ConfigurationBuilder::new(),
    )
    .unwrap();

    let alice_fragment = alice
        .transaction_to(
            &jormungandr.genesis_block_hash(),
            &jormungandr.fees(),
            bob.address(),
            100.into(),
        )
        .unwrap();

    let bob_fragment = bob
        .transaction_to(
            &jormungandr.genesis_block_hash(),
            &jormungandr.fees(),
            alice.address(),
            100.into(),
        )
        .unwrap();
    let clarice_fragment = clarice
        .transaction_to(
            &jormungandr.genesis_block_hash(),
            &jormungandr.fees(),
            alice.address(),
            100.into(),
        )
        .unwrap();

    let david_fragment = david
        .transaction_to(
            &jormungandr.genesis_block_hash(),
            &jormungandr.fees(),
            alice.address(),
            100.into(),
        )
        .unwrap();
    (
        jormungandr,
        alice_fragment,
        bob_fragment,
        clarice_fragment,
        david_fragment,
    )
}

#[rstest]
pub fn fail_fast_on_all_valid(world: (JormungandrProcess, Fragment, Fragment, Fragment, Fragment)) {
    let (jormungandr, valid_fragment_1, valid_fragment_2, valid_fragment_3, _) = world;
    let transaction_sender = jormungandr.fragment_sender(FragmentSenderSetup::resend_3_times());
    let tx_ids = transaction_sender
        .send_batch_fragments(
            vec![valid_fragment_1, valid_fragment_2, valid_fragment_3],
            true,
            &jormungandr,
        )
        .unwrap();

    let verifier = FragmentVerifier;
    verifier
        .wait_for_all_fragments(Duration::from_secs(5), &jormungandr)
        .unwrap();

    assert_all_valid(&tx_ids, &jormungandr);
}

#[rstest]
pub fn fail_fast_off_all_valid(
    world: (JormungandrProcess, Fragment, Fragment, Fragment, Fragment),
) {
    let (jormungandr, valid_fragment_1, valid_fragment_2, valid_fragment_3, _) = world;
    let transaction_sender = jormungandr.fragment_sender(FragmentSenderSetup::resend_3_times());
    let tx_ids = transaction_sender
        .send_batch_fragments(
            vec![valid_fragment_1, valid_fragment_2, valid_fragment_3],
            false,
            &jormungandr,
        )
        .unwrap();

    let verifier = FragmentVerifier;
    verifier
        .wait_for_all_fragments(Duration::from_secs(5), &jormungandr)
        .unwrap();

    assert_all_valid(&tx_ids, &jormungandr);
}

#[rstest]
pub fn fail_fast_on_first_invalid(
    world: (JormungandrProcess, Fragment, Fragment, Fragment, Fragment),
) {
    let (jormungandr, valid_fragment_1, valid_fragment_2, _, invalid_fragment) = world;
    let transaction_sender = jormungandr.fragment_sender(FragmentSenderSetup::resend_3_times());
    let tx_ids = transaction_sender
        .send_batch_fragments(
            vec![invalid_fragment, valid_fragment_1, valid_fragment_2],
            true,
            &jormungandr,
        )
        .unwrap();

    let verifier = FragmentVerifier;
    println!(
        "{:#?}",
        verifier
            .wait_for_all_fragments(Duration::from_secs(5), &jormungandr)
            .unwrap()
    );

    assert_all_invalid(&tx_ids, &jormungandr);
}

#[rstest]
pub fn fail_fast_off_first_invalid(
    world: (JormungandrProcess, Fragment, Fragment, Fragment, Fragment),
) {
    let (jormungandr, valid_fragment_1, valid_fragment_2, _, invalid_fragment) = world;
    let transaction_sender = jormungandr.fragment_sender(FragmentSenderSetup::resend_3_times());
    let tx_ids = transaction_sender
        .send_batch_fragments(
            vec![invalid_fragment, valid_fragment_1, valid_fragment_2],
            false,
            &jormungandr,
        )
        .unwrap();

    let verifier = FragmentVerifier;
    verifier
        .wait_for_all_fragments(Duration::from_secs(5), &jormungandr)
        .unwrap();

    assert_invalid(&tx_ids[0], &jormungandr);
    assert_valid(&tx_ids[1], &jormungandr);
    assert_valid(&tx_ids[2], &jormungandr);
}

#[rstest]
pub fn fail_fast_off_invalid_in_middle(
    world: (JormungandrProcess, Fragment, Fragment, Fragment, Fragment),
) {
    let (jormungandr, valid_fragment_1, valid_fragment_2, _, invalid_fragment) = world;
    let transaction_sender = jormungandr.fragment_sender(FragmentSenderSetup::resend_3_times());
    let tx_ids = transaction_sender
        .send_batch_fragments(
            vec![valid_fragment_1, invalid_fragment, valid_fragment_2],
            false,
            &jormungandr,
        )
        .unwrap();

    let verifier = FragmentVerifier;
    verifier
        .wait_for_all_fragments(Duration::from_secs(5), &jormungandr)
        .unwrap();

    assert_valid(&tx_ids[0], &jormungandr);
    assert_valid(&tx_ids[2], &jormungandr);
    assert_invalid(&tx_ids[1], &jormungandr);
}

#[rstest]
pub fn fail_fast_on_invalid_in_middle(
    world: (JormungandrProcess, Fragment, Fragment, Fragment, Fragment),
) {
    let (jormungandr, valid_fragment_1, valid_fragment_2, _, invalid_fragment) = world;
    let transaction_sender = jormungandr.fragment_sender(FragmentSenderSetup::resend_3_times());
    let tx_ids = transaction_sender
        .send_batch_fragments(
            vec![valid_fragment_1, invalid_fragment, valid_fragment_2],
            true,
            &jormungandr,
        )
        .unwrap();

    let verifier = FragmentVerifier;
    verifier
        .wait_for_all_fragments(Duration::from_secs(5), &jormungandr)
        .unwrap();

    assert_valid(&tx_ids[0], &jormungandr);
    assert_invalid(&tx_ids[1], &jormungandr);
    assert_invalid(&tx_ids[2], &jormungandr);
}
#[rstest]
pub fn fail_fast_on_last_invalid(
    world: (JormungandrProcess, Fragment, Fragment, Fragment, Fragment),
) {
    let (jormungandr, valid_fragment_1, valid_fragment_2, _, invalid_fragment) = world;
    let transaction_sender = jormungandr.fragment_sender(FragmentSenderSetup::resend_3_times());
    let tx_ids = transaction_sender
        .send_batch_fragments(
            vec![valid_fragment_1, valid_fragment_2, invalid_fragment],
            true,
            &jormungandr,
        )
        .unwrap();

    let verifier = FragmentVerifier;
    verifier
        .wait_for_all_fragments(Duration::from_secs(5), &jormungandr)
        .unwrap();

    assert_valid(&tx_ids[0], &jormungandr);
    assert_valid(&tx_ids[1], &jormungandr);
    assert_invalid(&tx_ids[2], &jormungandr);
}

#[rstest]
pub fn fail_fast_off_last_invalid(
    world: (JormungandrProcess, Fragment, Fragment, Fragment, Fragment),
) {
    let (jormungandr, valid_fragment_1, valid_fragment_2, _, invalid_fragment) = world;
    let transaction_sender = jormungandr.fragment_sender(FragmentSenderSetup::resend_3_times());
    let tx_ids = transaction_sender
        .send_batch_fragments(
            vec![valid_fragment_1, valid_fragment_2, invalid_fragment],
            false,
            &jormungandr,
        )
        .unwrap();

    let verifier = FragmentVerifier;
    verifier
        .wait_for_all_fragments(Duration::from_secs(5), &jormungandr)
        .unwrap();

    assert_valid(&tx_ids[0], &jormungandr);
    assert_valid(&tx_ids[1], &jormungandr);
    assert_invalid(&tx_ids[2], &jormungandr);
}

pub fn assert_all_valid(mem_pool_checks: &[MemPoolCheck], jormungandr: &JormungandrProcess) {
    let ids: Vec<String> = mem_pool_checks
        .iter()
        .map(|x| x.fragment_id().to_string())
        .collect();
    let statuses = jormungandr.rest().fragments_statuses(ids.clone()).unwrap();

    assert_eq!(ids.len(), statuses.len());

    ids.iter()
        .for_each(|id| match statuses.get(&id.to_string()) {
            Some(status) => assert_in_block(status),
            None => panic!("{} not found", id.to_string()),
        })
}

pub fn assert_all_invalid(mem_pool_checks: &[MemPoolCheck], jormungandr: &JormungandrProcess) {
    let ids: Vec<String> = mem_pool_checks
        .iter()
        .map(|x| x.fragment_id().to_string())
        .collect();
    let statuses = jormungandr.rest().fragments_statuses(ids.clone()).unwrap();

    assert_eq!(ids.len(), statuses.len());

    ids.iter()
        .for_each(|id| match statuses.get(&id.to_string()) {
            Some(status) => assert_not_in_block(status),
            None => panic!("{} not found", id.to_string()),
        })
}

pub fn assert_valid(mem_pool_check: &MemPoolCheck, jormungandr: &JormungandrProcess) {
    let ids = vec![mem_pool_check.fragment_id().to_string()];

    let statuses = jormungandr.rest().fragments_statuses(ids.clone()).unwrap();

    assert_eq!(ids.len(), statuses.len());

    ids.iter().for_each(|id| match statuses.get(id) {
        Some(status) => assert_in_block(status),
        None => panic!("{} not found", id.to_string()),
    })
}

pub fn assert_invalid(mem_pool_check: &MemPoolCheck, jormungandr: &JormungandrProcess) {
    let ids = vec![mem_pool_check.fragment_id().to_string()];
    let statuses = jormungandr.rest().fragments_statuses(ids.clone()).unwrap();

    assert_eq!(ids.len(), statuses.len());

    ids.iter().for_each(|id| match statuses.get(id) {
        Some(status) => assert_not_in_block(status),
        None => panic!("{} not found", id.to_string()),
    })
}
