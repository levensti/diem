// Copyright (c) The Diem Core Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::{
    data_notification::DataPayload,
    error::Error,
    streaming_client::{
        new_streaming_service_client_listener_pair, DataStreamingClient, PayloadRefetchReason,
        StreamingServiceClient,
    },
    streaming_service::DataStreamingService,
    tests::utils::{
        MockDiemDataClient, MAX_ADVERTISED_EPOCH, MAX_NOTIFICATION_TIMEOUT_SECS,
        MIN_ADVERTISED_EPOCH,
    },
};
use claim::{assert_le, assert_matches, assert_ok};
use futures::StreamExt;
use std::time::Duration;
use tokio::time::timeout;

#[tokio::test]
async fn test_notifications_epoch_ending() {
    // Create a new streaming client and service
    let (streaming_client, streaming_service) = create_new_streaming_client_and_service();
    tokio::spawn(streaming_service.start_service());

    // Request an epoch ending stream and get a data stream listener
    let mut stream_listener = streaming_client
        .get_all_epoch_ending_ledger_infos(MIN_ADVERTISED_EPOCH)
        .await
        .unwrap();

    // Read the data notifications from the stream and verify epoch ordering
    let mut next_expected_epoch = MIN_ADVERTISED_EPOCH;
    let expected_end_epoch = MAX_ADVERTISED_EPOCH - 1;
    loop {
        if let Ok(data_notification) = timeout(
            Duration::from_secs(MAX_NOTIFICATION_TIMEOUT_SECS),
            stream_listener.select_next_some(),
        )
        .await
        {
            if let DataPayload::EpochEndingLedgerInfos(ledger_infos_with_sigs) =
                data_notification.data_payload
            {
                for ledger_info_with_sigs in ledger_infos_with_sigs {
                    let epoch = ledger_info_with_sigs.ledger_info().commit_info().epoch();
                    assert_eq!(next_expected_epoch, epoch);
                    assert_le!(epoch, expected_end_epoch);
                    next_expected_epoch += 1;
                }
            } else {
                panic!(
                    "Expected an epoch ending ledger info payload, but got: {:?}",
                    data_notification
                );
            }
        } else {
            if next_expected_epoch == expected_end_epoch + 1 {
                return; // We hit the end of the stream!
            }
            panic!(
                "Timed out waiting for a data notification! Next expected epoch: {:?}",
                next_expected_epoch
            );
        }
    }
}

#[tokio::test]
async fn test_stream_epoch_ending() {
    // Create a new streaming client and service
    let (streaming_client, streaming_service) = create_new_streaming_client_and_service();
    tokio::spawn(streaming_service.start_service());

    // Request an epoch ending stream and verify we get a data stream listener
    let streaming_client_clone = streaming_client.clone();
    let result = streaming_client_clone
        .get_all_epoch_ending_ledger_infos(100)
        .await;
    assert_ok!(result);

    // Try to request a stream where epoch data is missing (all data was pruned at version 100)
    let streaming_client_clone = streaming_client.clone();
    let result = streaming_client_clone
        .get_all_epoch_ending_ledger_infos(0)
        .await;
    assert_matches!(result, Err(Error::DataIsUnavailable(_)));

    // Try to request a stream where epoch data is missing (we are higher than anything advertised)
    let result = streaming_client
        .get_all_epoch_ending_ledger_infos(10000)
        .await;
    assert_matches!(result, Err(Error::DataIsUnavailable(_)));
}

#[tokio::test]
async fn test_stream_unsupported() {
    // Create a new streaming client and service
    let (streaming_client, streaming_service) = create_new_streaming_client_and_service();
    tokio::spawn(streaming_service.start_service());

    // Request an account stream and verify it's unsupported
    let streaming_client_clone = streaming_client.clone();
    let result = streaming_client_clone.get_all_accounts(0).await;
    assert_matches!(result, Err(Error::UnsupportedRequestEncountered(_)));

    // Request a transaction stream and verify it's unsupported
    let streaming_client_clone = streaming_client.clone();
    let result = streaming_client_clone
        .get_all_transactions(0, 100, 200, true)
        .await;
    assert_matches!(result, Err(Error::UnsupportedRequestEncountered(_)));

    // Request a transaction output stream and verify it's unsupported
    let streaming_client_clone = streaming_client.clone();
    let result = streaming_client_clone
        .get_all_transaction_outputs(0, 100, 200)
        .await;
    assert_matches!(result, Err(Error::UnsupportedRequestEncountered(_)));

    // Request a continuous transaction stream and verify it's unsupported
    let streaming_client_clone = streaming_client.clone();
    let result = streaming_client_clone
        .continuously_stream_transactions(0, 0, true)
        .await;
    assert_matches!(result, Err(Error::UnsupportedRequestEncountered(_)));

    // Request a continuous transaction output stream and verify it's unsupported
    let streaming_client_clone = streaming_client.clone();
    let result = streaming_client_clone
        .continuously_stream_transaction_outputs(0, 0)
        .await;
    assert_matches!(result, Err(Error::UnsupportedRequestEncountered(_)));

    // Request a refetch notification payload stream and verify it's unsupported
    let result = streaming_client
        .refetch_notification_payload(0, PayloadRefetchReason::InvalidPayloadData)
        .await;
    assert_matches!(result, Err(Error::UnsupportedRequestEncountered(_)));
}

fn create_new_streaming_client_and_service() -> (
    StreamingServiceClient,
    DataStreamingService<MockDiemDataClient>,
) {
    // Create a new streaming client and listener
    let (streaming_client, streaming_service_listener) =
        new_streaming_service_client_listener_pair();

    // Create the streaming service and connect it to the listener
    let diem_data_client = MockDiemDataClient {};
    let streaming_service = DataStreamingService::new(diem_data_client, streaming_service_listener);

    (streaming_client, streaming_service)
}
