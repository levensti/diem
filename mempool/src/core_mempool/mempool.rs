// Copyright (c) The Diem Core Contributors
// SPDX-License-Identifier: Apache-2.0

//! Mempool is used to track transactions which have been submitted but not yet
//! agreed upon.
use crate::{
    core_mempool::{
        index::TxnPointer,
        transaction::{MempoolTransaction, TimelineState},
        transaction_store::TransactionStore,
        ttl_cache::TtlCache,
    },
    counters,
    logging::{LogEntry, LogSchema, TxnsLog},
};
use diem_config::config::NodeConfig;
use diem_crypto::HashValue;
use diem_logger::prelude::*;
use diem_types::{
    account_address::AccountAddress,
    account_config::AccountSequenceInfo,
    mempool_status::{MempoolStatus, MempoolStatusCode},
    transaction::{GovernanceRole, SignedTransaction},
};
use std::{
    cmp::max,
    collections::HashSet,
    time::{Duration, SystemTime},
};

pub struct Mempool {
    // Stores the metadata of all transactions in mempool (of all states).
    transactions: TransactionStore,

    sequence_number_cache: TtlCache<AccountAddress, u64>,
    // For each transaction, an entry with a timestamp is added when the transaction enters mempool.
    // This is used to measure e2e latency of transactions in the system, as well as the time it
    // takes to pick it up by consensus.
    pub(crate) metrics_cache: TtlCache<(AccountAddress, u64), SystemTime>,
    pub system_transaction_timeout: Duration,
}

impl Mempool {
    pub fn new(config: &NodeConfig) -> Self {
        Mempool {
            transactions: TransactionStore::new(&config.mempool),
            sequence_number_cache: TtlCache::new(config.mempool.capacity, Duration::from_secs(100)),
            metrics_cache: TtlCache::new(config.mempool.capacity, Duration::from_secs(100)),
            system_transaction_timeout: Duration::from_secs(
                config.mempool.system_transaction_timeout_secs,
            ),
        }
    }

    /// This function will be called once the transaction has been stored.
    pub(crate) fn remove_transaction(
        &mut self,
        sender: &AccountAddress,
        sequence_number: u64,
        is_rejected: bool,
    ) {
        trace!(
            LogSchema::new(LogEntry::RemoveTxn).txns(TxnsLog::new_txn(*sender, sequence_number)),
            is_rejected = is_rejected
        );
        let metric_label = if is_rejected {
            counters::COMMIT_REJECTED_LABEL
        } else {
            counters::COMMIT_ACCEPTED_LABEL
        };
        self.log_latency(*sender, sequence_number, metric_label);
        self.metrics_cache.remove(&(*sender, sequence_number));

        let current_seq_number = self
            .sequence_number_cache
            .remove(sender)
            .unwrap_or_default();

        if is_rejected {
            if sequence_number >= current_seq_number {
                self.transactions
                    .reject_transaction(sender, sequence_number);
            }
        } else {
            let new_seq_number = max(current_seq_number, sequence_number + 1);
            self.sequence_number_cache.insert(*sender, new_seq_number);

            let new_seq_number = if let Some(mempool_transaction) =
                self.transactions.get_mempool_txn(sender, sequence_number)
            {
                match mempool_transaction
                    .sequence_info
                    .account_sequence_number_type
                {
                    // In the CRSN case, we can only clear out transactions based on the LHS of the
                    // window (i.e., min_nonce).
                    x @ AccountSequenceInfo::CRSN { .. } => x,
                    AccountSequenceInfo::Sequential(_) => {
                        AccountSequenceInfo::Sequential(new_seq_number)
                    }
                }
            } else {
                AccountSequenceInfo::Sequential(new_seq_number)
            };
            // update current cached sequence number for account
            self.sequence_number_cache
                .insert(*sender, new_seq_number.min_seq());
            self.transactions.commit_transaction(sender, new_seq_number);
        }
    }

    fn log_latency(&mut self, account: AccountAddress, sequence_number: u64, metric: &str) {
        if let Some(&creation_time) = self.metrics_cache.get(&(account, sequence_number)) {
            if let Ok(time_delta) = SystemTime::now().duration_since(creation_time) {
                counters::CORE_MEMPOOL_TXN_COMMIT_LATENCY
                    .with_label_values(&[metric])
                    .observe(time_delta.as_secs_f64());
            }
        }
    }

    pub(crate) fn get_by_hash(&self, hash: HashValue) -> Option<SignedTransaction> {
        self.transactions.get_by_hash(hash)
    }

    /// Used to add a transaction to the Mempool.
    /// Performs basic validation: checks account's sequence number.
    pub(crate) fn add_txn(
        &mut self,
        txn: SignedTransaction,
        gas_amount: u64,
        ranking_score: u64,
        crsn_or_seqno: AccountSequenceInfo,
        timeline_state: TimelineState,
        governance_role: GovernanceRole,
    ) -> MempoolStatus {
        let db_sequence_number = crsn_or_seqno.min_seq();
        trace!(
            LogSchema::new(LogEntry::AddTxn)
                .txns(TxnsLog::new_txn(txn.sender(), txn.sequence_number())),
            committed_seq_number = db_sequence_number
        );
        let cached_value = self.sequence_number_cache.get(&txn.sender());
        let sequence_number = match crsn_or_seqno {
            AccountSequenceInfo::CRSN { .. } => crsn_or_seqno,
            AccountSequenceInfo::Sequential(_) => AccountSequenceInfo::Sequential(
                cached_value.map_or(db_sequence_number, |value| max(*value, db_sequence_number)),
            ),
        };
        self.sequence_number_cache
            .insert(txn.sender(), sequence_number.min_seq());

        // don't accept old transactions (e.g. seq is less than account's current seq_number)
        if txn.sequence_number() < sequence_number.min_seq() {
            return MempoolStatus::new(MempoolStatusCode::InvalidSeqNumber).with_message(format!(
                "transaction sequence number is {}, current sequence number is  {}",
                txn.sequence_number(),
                sequence_number.min_seq(),
            ));
        }

        let expiration_time =
            diem_infallible::duration_since_epoch() + self.system_transaction_timeout;
        if timeline_state != TimelineState::NonQualified {
            self.metrics_cache
                .insert((txn.sender(), txn.sequence_number()), SystemTime::now());
        }

        let txn_info = MempoolTransaction::new(
            txn,
            expiration_time,
            gas_amount,
            ranking_score,
            timeline_state,
            governance_role,
            sequence_number,
        );

        self.transactions.insert(txn_info)
    }

    /// Fetches next block of transactions for consensus.
    /// `batch_size` - size of requested block.
    /// `seen_txns` - transactions that were sent to Consensus but were not committed yet,
    ///  mempool should filter out such transactions.
    #[allow(clippy::explicit_counter_loop)]
    pub(crate) fn get_block(
        &mut self,
        batch_size: u64,
        mut seen: HashSet<TxnPointer>,
    ) -> Vec<SignedTransaction> {
        let mut result = vec![];
        // Helper DS. Helps to mitigate scenarios where account submits several transactions
        // with increasing gas price (e.g. user submits transactions with sequence number 1, 2
        // and gas_price 1, 10 respectively)
        // Later txn has higher gas price and will be observed first in priority index iterator,
        // but can't be executed before first txn. Once observed, such txn will be saved in
        // `skipped` DS and rechecked once it's ancestor becomes available
        let mut skipped = HashSet::new();
        let seen_size = seen.len();
        let mut txn_walked = 0usize;
        // iterate over the queue of transactions based on gas price
        'main: for txn in self.transactions.iter_queue() {
            txn_walked += 1;
            if seen.contains(&TxnPointer::from(txn)) {
                continue;
            }
            let account_seqtype = txn.sequence_number.account_sequence_number_type;
            let tx_seq = txn.sequence_number.transaction_sequence_number;
            let account_sequence_number = self.sequence_number_cache.get(&txn.address);
            let seen_previous = tx_seq > 0 && seen.contains(&(txn.address, tx_seq - 1));
            // include transaction if it's "next" for given account or
            // we've already sent its ancestor to Consensus. In the case of CRSNs, we can safely
            // assume that it can be included.
            if seen_previous
                || account_sequence_number == Some(&tx_seq)
                || matches!(account_seqtype, AccountSequenceInfo::CRSN { .. })
            {
                let ptr = TxnPointer::from(txn);
                seen.insert(ptr);
                result.push(ptr);
                if (result.len() as u64) == batch_size {
                    break;
                }

                // check if we can now include some transactions
                // that were skipped before for given account
                let mut skipped_txn = (txn.address, tx_seq + 1);
                while skipped.contains(&skipped_txn) {
                    seen.insert(skipped_txn);
                    result.push(skipped_txn);
                    if (result.len() as u64) == batch_size {
                        break 'main;
                    }
                    skipped_txn = (txn.address, skipped_txn.1 + 1);
                }
            } else {
                skipped.insert(TxnPointer::from(txn));
            }
        }
        let result_size = result.len();
        // convert transaction pointers to real values
        let mut block_log = TxnsLog::new();
        let block: Vec<_> = result
            .into_iter()
            .filter_map(|(address, tx_seq)| {
                block_log.add(address, tx_seq);
                self.transactions.get(&address, tx_seq)
            })
            .collect();

        debug!(
            LogSchema::new(LogEntry::GetBlock).txns(block_log),
            seen_consensus = seen_size,
            walked = txn_walked,
            seen_after = seen.len(),
            result_size = result_size,
            block_size = block.len()
        );
        for transaction in &block {
            self.log_latency(
                transaction.sender(),
                transaction.sequence_number(),
                counters::GET_BLOCK_STAGE_LABEL,
            );
        }
        block
    }

    /// Periodic core mempool garbage collection.
    /// Removes all expired transactions and clears expired entries in metrics
    /// cache and sequence number cache.
    pub(crate) fn gc(&mut self) {
        let now = SystemTime::now();
        self.transactions.gc_by_system_ttl(&self.metrics_cache);
        self.metrics_cache.gc(now);
        self.sequence_number_cache.gc(now);
    }

    /// Garbage collection based on client-specified expiration time.
    pub(crate) fn gc_by_expiration_time(&mut self, block_time: Duration) {
        self.transactions
            .gc_by_expiration_time(block_time, &self.metrics_cache);
    }

    /// Read `count` transactions from timeline since `timeline_id`.
    /// Returns block of transactions and new last_timeline_id.
    pub(crate) fn read_timeline(
        &mut self,
        timeline_id: u64,
        count: usize,
    ) -> (Vec<SignedTransaction>, u64) {
        self.transactions.read_timeline(timeline_id, count)
    }

    /// Read transactions from timeline from `start_id` (exclusive) to `end_id` (inclusive).
    pub(crate) fn timeline_range(&mut self, start_id: u64, end_id: u64) -> Vec<SignedTransaction> {
        self.transactions.timeline_range(start_id, end_id)
    }

    pub fn gen_snapshot(&self) -> TxnsLog {
        self.transactions.gen_snapshot(&self.metrics_cache)
    }

    #[cfg(test)]
    pub fn get_parking_lot_size(&self) -> usize {
        self.transactions.get_parking_lot_size()
    }
}
