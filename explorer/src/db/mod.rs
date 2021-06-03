pub mod error;
pub mod indexing;
pub mod multiverse;
pub mod persistent_sequence;

use self::error::{ExplorerError as Error, Result};
use self::indexing::{
    Addresses, Blocks, ChainLengths, EpochData, Epochs, ExplorerAddress, ExplorerBlock, StakePool,
    StakePoolBlocks, StakePoolData, Transactions,
};
use self::persistent_sequence::PersistentSequence;
use chain_core::property::Block as _;
pub use multiverse::Ref;

use chain_addr::Discrimination;
use chain_impl_mockchain::block::HeaderId as HeaderHash;
use chain_impl_mockchain::fee::LinearFee;
use chain_impl_mockchain::{
    block::{Block, ChainLength, Epoch},
    certificate::{Certificate, PoolId},
    chaintypes::ConsensusVersion,
    config::ConfigParam,
    fragment::{ConfigParams, Fragment, FragmentId},
};
use futures::prelude::*;
use multiverse::Multiverse;
use std::convert::Infallible;
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};
use tokio::sync::{broadcast, RwLock};

#[derive(Clone)]
pub struct Explorer {
    pub db: ExplorerDb,
}

#[derive(Clone)]
struct Tip(Arc<RwLock<HeaderHash>>);

#[derive(Clone)]
pub struct ExplorerDb {
    /// Structure that keeps all the known states to allow easy branch management
    /// each new block is indexed by getting its previous `State` from the multiverse
    /// and inserted a new updated one.
    multiverse: Multiverse,
    /// This keeps track of the longest chain seen until now. All the queries are
    /// performed using the state of this branch, the HeaderHash is used as key for the
    /// multiverse, and the ChainLength is used in the updating process.
    longest_chain_tip: Tip,
    pub blockchain_config: BlockchainConfig,
    stable_store: StableIndex,
    tip_broadcast: tokio::sync::broadcast::Sender<(HeaderHash, multiverse::Ref)>,
}

#[derive(Clone)]
pub struct StableIndex {
    confirmed_block_chain_length: Arc<AtomicU32>,
}

#[derive(Clone)]
pub struct BlockchainConfig {
    /// Used to construct `Address` from `AccountIndentifier` when processing transaction
    /// inputs
    pub discrimination: Discrimination,
    pub consensus_version: ConsensusVersion,
    pub fees: LinearFee,
    pub epoch_stability_depth: u32,
}

/// Inmutable data structure used to represent the explorer's state at a given Block
/// A new state can be obtained to from a Block and it's previous state, getting two
/// independent states but with memory sharing to minimize resource utilization
#[derive(Clone)]
pub struct State {
    pub transactions: Transactions,
    pub blocks: Blocks,
    addresses: Addresses,
    epochs: Epochs,
    chain_lengths: ChainLengths,
    stake_pool_data: StakePool,
    stake_pool_blocks: StakePoolBlocks,
}

#[derive(Clone)]
pub struct Settings {
    /// This is the prefix that's used for the Address bech32 string representation in the
    /// responses (in the queries any prefix can be used). base32 serialization could
    /// also be used, but the `Address` struct doesn't have a deserialization method right
    /// now
    pub address_bech32_prefix: String,
}

impl ExplorerDb {
    pub fn bootstrap(block0: Block) -> Result<Self> {
        let blockchain_config = BlockchainConfig::from_config_params(
            block0
                .contents
                .iter()
                .filter_map(|fragment| match fragment {
                    Fragment::Initial(config_params) => Some(config_params),
                    _ => None,
                })
                .next()
                .expect("the Initial fragment to be present in the genesis block"),
        );

        let block = ExplorerBlock::resolve_from(
            &block0,
            indexing::ExplorerBlockBuildingContext {
                discrimination: blockchain_config.discrimination,
                prev_transactions: &Transactions::new(),
                prev_blocks: &Blocks::new(),
            },
        );

        let blocks = apply_block_to_blocks(Blocks::new(), &block)?;
        let epochs = apply_block_to_epochs(Epochs::new(), &block);
        let chain_lengths = apply_block_to_chain_lengths(ChainLengths::new(), &block)?;
        let transactions = apply_block_to_transactions(Transactions::new(), &block)?;
        let addresses = apply_block_to_addresses(Addresses::new(), &block);
        let (stake_pool_data, stake_pool_blocks) =
            apply_block_to_stake_pools(StakePool::new(), StakePoolBlocks::new(), &block);

        let initial_state = State {
            transactions,
            blocks,
            epochs,
            chain_lengths,
            addresses,
            stake_pool_data,
            stake_pool_blocks,
        };

        let block0_id = block0.id();
        let (_, multiverse) = Multiverse::new(block0.chain_length(), block0_id, initial_state);

        let block0_id = block0.id();

        let (tx, _) = broadcast::channel(10);

        let bootstraped_db = ExplorerDb {
            multiverse,
            longest_chain_tip: Tip::new(block0_id),
            blockchain_config,
            stable_store: StableIndex {
                confirmed_block_chain_length: Arc::new(AtomicU32::default()),
            },
            tip_broadcast: tx,
        };

        Ok(bootstraped_db)
    }

    /// Try to add a new block to the indexes, this can fail if the parent of the block is
    /// not processed. Also, update the longest seen chain with this block as tip if its
    /// chain length is greater than the current.
    /// This doesn't perform any validation on the given block and the previous state, it
    /// is assumed that the Block is valid
    pub async fn apply_block(&self, block: Block) -> Result<multiverse::Ref> {
        let previous_block = block.header.block_parent_hash();
        let chain_length = block.header.chain_length();
        let block_id = block.header.hash();
        let multiverse = self.multiverse.clone();
        let discrimination = self.blockchain_config.discrimination;

        let previous_state = multiverse
            .get_ref(&previous_block)
            .await
            .ok_or_else(|| Error::AncestorNotFound(block.id()))?;
        let State {
            transactions,
            blocks,
            addresses,
            epochs,
            chain_lengths,
            stake_pool_data,
            stake_pool_blocks,
        } = previous_state.state().clone();

        let explorer_block = ExplorerBlock::resolve_from(
            &block,
            indexing::ExplorerBlockBuildingContext {
                discrimination,
                prev_transactions: &transactions,
                prev_blocks: &blocks,
            },
        );
        let (stake_pool_data, stake_pool_blocks) =
            apply_block_to_stake_pools(stake_pool_data, stake_pool_blocks, &explorer_block);

        let state_ref = multiverse
            .insert(
                chain_length,
                block.parent_id(),
                block_id,
                State {
                    transactions: apply_block_to_transactions(transactions, &explorer_block)?,
                    blocks: apply_block_to_blocks(blocks, &explorer_block)?,
                    addresses: apply_block_to_addresses(addresses, &explorer_block),
                    epochs: apply_block_to_epochs(epochs, &explorer_block),
                    chain_lengths: apply_block_to_chain_lengths(chain_lengths, &explorer_block)?,
                    stake_pool_data,
                    stake_pool_blocks,
                },
            )
            .await;

        Ok(state_ref)
    }

    pub async fn get_block(&self, block_id: &HeaderHash) -> Option<Arc<ExplorerBlock>> {
        for (_hash, state_ref) in self.multiverse.tips().await.iter() {
            if let Some(b) = state_ref.state().blocks.lookup(&block_id) {
                return Some(Arc::clone(b));
            }
        }

        None
    }

    pub async fn set_tip(&self, hash: HeaderHash) -> bool {
        // the tip changes which means now a block is confirmed (at least after
        // the initial epoch_stability_depth blocks).

        let state_ref = if let Some(state_ref) = self.multiverse.get_ref(&hash).await {
            state_ref
        } else {
            return false;
        };

        let state = state_ref.state();
        let block = Arc::clone(state.blocks.lookup(&hash).unwrap());

        if let Some(confirmed_block_chain_length) = block
            .chain_length()
            .nth_ancestor(self.blockchain_config.epoch_stability_depth)
        {
            debug_assert!(
                ChainLength::from(
                    self.stable_store
                        .confirmed_block_chain_length
                        .load(Ordering::Acquire)
                ) <= block.chain_length()
            );

            self.stable_store
                .confirmed_block_chain_length
                .store(confirmed_block_chain_length.into(), Ordering::Release);

            self.multiverse
                .gc(self.blockchain_config.epoch_stability_depth)
                .await;
        }

        let mut guard = self.longest_chain_tip.0.write().await;

        *guard = hash;

        let _ = self.tip_broadcast.send((hash, state_ref));

        true
    }

    pub async fn get_block_with_branches(
        &self,
        block_id: &HeaderHash,
    ) -> Option<(Arc<ExplorerBlock>, Vec<(HeaderHash, multiverse::Ref)>)> {
        let mut block = None;
        let mut tips = Vec::new();

        for (hash, state_ref) in self.multiverse.tips().await.drain(..) {
            if let Some(b) = state_ref.state().blocks.lookup(&block_id) {
                block = block.or_else(|| Some(Arc::clone(b)));
                tips.push((hash, state_ref));
            }
        }

        block.map(|b| (b, tips))
    }

    pub async fn get_epoch(&self, epoch: Epoch) -> Option<EpochData> {
        let tips = self.multiverse.tips().await;
        let (_, state_ref) = &tips[0];

        state_ref
            .state()
            .epochs
            .lookup(&epoch)
            .map(|e| e.as_ref().clone())
    }

    pub async fn is_block_confirmed(&self, block_id: &HeaderHash) -> bool {
        let current_branch = self
            .multiverse
            .get_ref(&self.longest_chain_tip.get_block_id().await)
            .await
            .unwrap();

        if let Some(block) = current_branch.state().blocks.lookup(&block_id) {
            let confirmed_block_chain_length: ChainLength = self
                .stable_store
                .confirmed_block_chain_length
                .load(Ordering::Acquire)
                .into();
            block.chain_length <= confirmed_block_chain_length
        } else {
            false
        }
    }

    pub async fn find_blocks_by_chain_length(&self, chain_length: ChainLength) -> Vec<HeaderHash> {
        let mut hashes = Vec::new();

        for (_hash, state_ref) in self.multiverse.tips().await.iter() {
            if let Some(hash) = state_ref.state().chain_lengths.lookup(&chain_length) {
                hashes.push(**hash);
            }
        }

        hashes.sort_unstable();
        hashes.dedup();

        hashes
    }

    pub async fn find_blocks_by_transaction(&self, transaction_id: &FragmentId) -> Vec<HeaderHash> {
        let mut txs: Vec<_> = self
            .multiverse
            .tips()
            .await
            .iter()
            .filter_map(|(_tip_hash, state_ref)| {
                state_ref
                    .state()
                    .transactions
                    .lookup(&transaction_id)
                    .map(|arc| *arc.clone())
            })
            .collect();

        txs.sort_unstable();
        txs.dedup();

        txs
    }

    pub async fn get_stake_pool_blocks(
        &self,
        pool: &PoolId,
    ) -> Option<Arc<PersistentSequence<HeaderHash>>> {
        let pool = pool.clone();

        // this is a tricky query, one option would be to take a hash and return
        // only the blocks from a particular branch, but it's not like a stake
        // pool would produce inconsistent branches itself, although there may
        // be a need to know the blocks that a stake pool got in the main branch
        // too.
        // for the time being, this query uses the maximum, because the branch
        // that has more blocks from this particular stake pool has all the
        // blocks produced by it
        self.multiverse
            .tips()
            .await
            .iter()
            .filter_map(|(_hash, state_ref)| state_ref.state().stake_pool_blocks.lookup(&pool))
            .max_by_key(|seq| seq.len())
            .map(Arc::clone)
    }

    pub async fn get_stake_pool_data(&self, pool: &PoolId) -> Option<Arc<StakePoolData>> {
        let pool = pool.clone();

        for (_hash, state_ref) in self.multiverse.tips().await.iter() {
            if let Some(b) = state_ref.state().stake_pool_data.lookup(&pool) {
                return Some(Arc::clone(b));
            }
        }

        None
    }

    pub async fn get_branch(&self, hash: &HeaderHash) -> Option<multiverse::Ref> {
        self.multiverse.get_ref(hash).await
    }

    pub async fn get_tip(&self) -> (HeaderHash, multiverse::Ref) {
        let hash = self.longest_chain_tip.get_block_id().await;
        (hash, self.multiverse.get_ref(&hash).await.unwrap())
    }

    pub async fn get_branches(&self) -> Vec<(HeaderHash, multiverse::Ref)> {
        self.multiverse.tips().await
    }

    pub fn tip_subscription(
        &self,
    ) -> impl Stream<
        Item = std::result::Result<
            (HeaderHash, multiverse::Ref),
            tokio_stream::wrappers::errors::BroadcastStreamRecvError,
        >,
    > {
        tokio_stream::wrappers::BroadcastStream::new(self.tip_broadcast.subscribe())
    }
}

fn apply_block_to_transactions(
    mut transactions: Transactions,
    block: &ExplorerBlock,
) -> Result<Transactions> {
    let block_id = block.id();
    let ids = block.transactions.values().map(|tx| tx.id());

    for id in ids {
        transactions = transactions
            .insert(id, Arc::new(block_id))
            .map_err(|_| Error::TransactionAlreadyExists(id))?;
    }

    Ok(transactions)
}

fn apply_block_to_blocks(blocks: Blocks, block: &ExplorerBlock) -> Result<Blocks> {
    let block_id = block.id();
    blocks
        .insert(block_id, Arc::new(block.clone()))
        .map_err(|_| Error::BlockAlreadyExists(block_id))
}

fn apply_block_to_addresses(mut addresses: Addresses, block: &ExplorerBlock) -> Addresses {
    let transactions = block.transactions.values();

    for tx in transactions {
        let id = tx.id();

        // A Hashset is used for preventing duplicates when the address is both an
        // input and an output in the given transaction

        let included_addresses: std::collections::HashSet<ExplorerAddress> = tx
            .outputs()
            .iter()
            .map(|output| output.address.clone())
            .chain(tx.inputs().iter().map(|input| input.address.clone()))
            .collect();

        for address in included_addresses {
            addresses = addresses.insert_or_update_simple(
                address,
                Arc::new(PersistentSequence::new().append(id)),
                |set| {
                    let new_set = set.append(id);
                    Some(Arc::new(new_set))
                },
            )
        }
    }
    addresses
}

fn apply_block_to_epochs(epochs: Epochs, block: &ExplorerBlock) -> Epochs {
    let epoch_id = block.date().epoch;
    let block_id = block.id();

    epochs.insert_or_update_simple(
        epoch_id,
        Arc::new(EpochData {
            first_block: block_id,
            last_block: block_id,
            total_blocks: 0,
        }),
        |data| {
            Some(Arc::new(EpochData {
                first_block: data.first_block,
                last_block: block_id,
                total_blocks: data.total_blocks + 1,
            }))
        },
    )
}

fn apply_block_to_chain_lengths(
    chain_lengths: ChainLengths,
    block: &ExplorerBlock,
) -> Result<ChainLengths> {
    let new_block_chain_length = block.chain_length();
    let new_block_hash = block.id();
    chain_lengths
        .insert(new_block_chain_length, Arc::new(new_block_hash))
        .map_err(|_| {
            // I think this shouldn't happen
            Error::ChainLengthBlockAlreadyExists(new_block_chain_length)
        })
}

fn apply_block_to_stake_pools(
    data: StakePool,
    blocks: StakePoolBlocks,
    block: &ExplorerBlock,
) -> (StakePool, StakePoolBlocks) {
    let mut blocks = match &block.producer() {
        indexing::BlockProducer::StakePool(id) => blocks
            .update(
                &id,
                |array: &Arc<PersistentSequence<HeaderHash>>| -> std::result::Result<_, Infallible> {
                    Ok(Some(Arc::new(array.append(block.id()))))
                },
            )
            .expect("block to be created by registered stake pool"),
        indexing::BlockProducer::BftLeader(_) => blocks,
        indexing::BlockProducer::None => blocks,
    };

    let mut data = data;

    for tx in block.transactions.values() {
        if let Some(cert) = &tx.certificate {
            blocks = match cert {
                Certificate::PoolRegistration(registration) => blocks
                    .insert(registration.to_id(), Arc::new(PersistentSequence::new()))
                    .expect("pool was registered more than once"),
                _ => blocks,
            };
            data = match cert {
                Certificate::PoolRegistration(registration) => data
                    .insert(
                        registration.to_id(),
                        Arc::new(StakePoolData {
                            registration: registration.clone(),
                            retirement: None,
                        }),
                    )
                    .expect("pool was registered more than once"),
                Certificate::PoolRetirement(retirement) => {
                    data.update::<_, Infallible>(&retirement.pool_id, |pool_data| {
                        Ok(Some(Arc::new(StakePoolData {
                            registration: pool_data.registration.clone(),
                            retirement: Some(retirement.clone()),
                        })))
                    })
                    .expect("pool was retired before registered");
                    data
                }
                _ => data,
            };
        }
    }

    (data, blocks)
}

impl BlockchainConfig {
    fn from_config_params(params: &ConfigParams) -> BlockchainConfig {
        let mut discrimination: Option<Discrimination> = None;
        let mut consensus_version: Option<ConsensusVersion> = None;
        let mut fees: Option<LinearFee> = None;
        let mut epoch_stability_depth: Option<u32> = None;

        for p in params.iter() {
            match p {
                ConfigParam::Discrimination(d) => {
                    discrimination.replace(*d);
                }
                ConfigParam::ConsensusVersion(v) => {
                    consensus_version.replace(*v);
                }
                ConfigParam::LinearFee(fee) => {
                    fees.replace(*fee);
                }
                ConfigParam::EpochStabilityDepth(d) => {
                    epoch_stability_depth.replace(*d);
                }
                _ => (),
            }
        }

        BlockchainConfig {
            discrimination: discrimination.expect("discrimination not found in initial params"),
            consensus_version: consensus_version
                .expect("consensus version not found in initial params"),
            fees: fees.expect("fees not found in initial params"),
            epoch_stability_depth: epoch_stability_depth
                .expect("epoch stability depth not found in initial params"),
        }
    }
}

impl Tip {
    fn new(block0_hash: HeaderHash) -> Tip {
        Tip(Arc::new(RwLock::new(block0_hash)))
    }

    async fn get_block_id(&self) -> HeaderHash {
        *self.0.read().await
    }
}

impl State {
    pub fn get_stake_pools(&self) -> Vec<(PoolId, Arc<StakePoolData>)> {
        self.stake_pool_data
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn transactions_by_address(
        &self,
        address: &ExplorerAddress,
    ) -> Option<PersistentSequence<FragmentId>> {
        self.addresses
            .lookup(address)
            .map(|txs| PersistentSequence::clone(txs))
    }

    // Get the hashes of all blocks in the range [from, to)
    // the ChainLength is returned to for easy of use in the case where
    // `to` is greater than the max
    pub fn get_block_hash_range(
        &self,
        from: ChainLength,
        to: ChainLength,
    ) -> Vec<(HeaderHash, ChainLength)> {
        let from = u32::from(from);
        let to = u32::from(to);

        (from..to)
            .filter_map(|i| {
                self.chain_lengths
                    .lookup(&i.into())
                    .map(|b| (*b.as_ref(), i.into()))
            })
            .collect()
    }
}
