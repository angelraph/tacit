//! In-memory compact block cache.
//!
//! The sync engine downloads compact blocks into a cache, scans them, then
//! drops them. Keeping that cache in memory means no second database on disk
//! and nothing that stops the same code running in a browser later.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use zcash_client_backend::data_api::chain::{error, BlockCache, BlockSource};
use zcash_client_backend::data_api::scanning::ScanRange;
use zcash_client_backend::proto::compact_formats::CompactBlock;
use zcash_protocol::consensus::BlockHeight;

#[derive(Debug, thiserror::Error)]
#[error("compact block cache is missing block {0}")]
pub struct MissingBlock(u32);

#[derive(Default)]
pub struct MemBlockCache {
    blocks: Mutex<BTreeMap<u32, CompactBlock>>,
}

impl MemBlockCache {
    pub fn new() -> Self {
        Self::default()
    }
}

fn height_of(b: &CompactBlock) -> u32 {
    b.height as u32
}

impl BlockSource for MemBlockCache {
    type Error = MissingBlock;

    fn with_blocks<F, WalletErrT>(
        &self,
        from_height: Option<BlockHeight>,
        limit: Option<usize>,
        mut with_block: F,
    ) -> Result<(), error::Error<WalletErrT, Self::Error>>
    where
        F: FnMut(CompactBlock) -> Result<(), error::Error<WalletErrT, Self::Error>>,
    {
        let start = from_height.map(u32::from).unwrap_or(0);
        let blocks: Vec<CompactBlock> = self
            .blocks
            .lock()
            .unwrap()
            .range(start..)
            .take(limit.unwrap_or(usize::MAX))
            .map(|(_, b)| b.clone())
            .collect();
        for b in blocks {
            with_block(b)?;
        }
        Ok(())
    }
}

#[async_trait]
impl BlockCache for MemBlockCache {
    fn get_tip_height(&self, range: Option<&ScanRange>) -> Result<Option<BlockHeight>, Self::Error> {
        let blocks = self.blocks.lock().unwrap();
        let tip = match range {
            Some(r) => {
                let (start, end) = (u32::from(r.block_range().start), u32::from(r.block_range().end));
                blocks.range(start..end).next_back().map(|(h, _)| *h)
            }
            None => blocks.keys().next_back().copied(),
        };
        Ok(tip.map(BlockHeight::from_u32))
    }

    async fn read(&self, range: &ScanRange) -> Result<Vec<CompactBlock>, Self::Error> {
        let (start, end) = (u32::from(range.block_range().start), u32::from(range.block_range().end));
        let blocks = self.blocks.lock().unwrap();
        let mut out = Vec::new();
        for h in start..end {
            match blocks.get(&h) {
                Some(b) => out.push(b.clone()),
                // Short reads are allowed as long as what we return is contiguous.
                None if !out.is_empty() => break,
                None => return Err(MissingBlock(h)),
            }
        }
        Ok(out)
    }

    async fn insert(&self, compact_blocks: Vec<CompactBlock>) -> Result<(), Self::Error> {
        let mut blocks = self.blocks.lock().unwrap();
        for b in compact_blocks {
            blocks.insert(height_of(&b), b);
        }
        Ok(())
    }

    async fn delete(&self, range: ScanRange) -> Result<(), Self::Error> {
        let (start, end) = (u32::from(range.block_range().start), u32::from(range.block_range().end));
        self.blocks.lock().unwrap().retain(|h, _| !(start..end).contains(h));
        Ok(())
    }
}
