// Copyright (c) The Diem Core Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::{experimental::pipeline_phase::StatelessPipeline, state_replication::StateComputer};
use anyhow::Result;
use async_trait::async_trait;
use consensus_types::executed_block::ExecutedBlock;
use diem_crypto::HashValue;
use executor_types::Error as ExecutionError;
use std::{
    fmt::{Debug, Display, Formatter},
    sync::Arc,
};

/// [ This class is used when consensus.decoupled = true ]
/// ExecutionPhase is a singleton that receives ordered blocks from
/// the buffer manager and execute them. After the execution is done,
/// ExecutionPhase sends the ordered blocks back to the buffer manager.
///

pub struct ExecutionRequest {
    pub ordered_blocks: Vec<ExecutedBlock>,
}

impl Debug for ExecutionRequest {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "{}", self)
    }
}

impl Display for ExecutionRequest {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "ExecutionRequest({:?})", self.ordered_blocks)
    }
}

pub struct ExecutionResponse {
    pub block_id: HashValue,
    pub inner: Result<Vec<ExecutedBlock>, ExecutionError>,
}

pub struct ExecutionPhase {
    execution_proxy: Arc<dyn StateComputer>,
}

impl ExecutionPhase {
    pub fn new(execution_proxy: Arc<dyn StateComputer>) -> Self {
        Self { execution_proxy }
    }
}

#[async_trait]
impl StatelessPipeline for ExecutionPhase {
    type Request = ExecutionRequest;
    type Response = ExecutionResponse;
    async fn process(&self, req: ExecutionRequest) -> ExecutionResponse {
        let ExecutionRequest { ordered_blocks } = req;
        let block_id = ordered_blocks.last().unwrap().id();

        // execute the blocks with execution_correctness_client
        let inner = ordered_blocks
            .iter()
            .map(|b| {
                let state_compute_result =
                    self.execution_proxy.compute(b.block(), b.parent_id())?;
                Ok(ExecutedBlock::new(b.block().clone(), state_compute_result))
            })
            .collect::<Result<Vec<ExecutedBlock>, ExecutionError>>();

        ExecutionResponse { block_id, inner }
    }
}
