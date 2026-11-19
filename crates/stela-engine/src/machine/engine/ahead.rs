//! Translation ahead of execution: the static successors of a block a fetch
//! compiled, in its own fetch page, are prepared by PEs whose guest waits,
//! before any PE fetches them.
//!
//! A fetch that compiles a block offers it to the store
//! ([`super::Translations::offer`]) with the entry PCs of its static
//! successors. The store turns those in the block's fetch page into
//! [`Job`]s and keeps them in a bounded [`Queue`], newest first. A PE whose
//! guest waits takes jobs in bounded steps: it reads the job's frame, checks
//! that its bytes still hash to the page key, and [`prepare`]s the block as
//! a fetch would compile it ([`super::Preparation::prepare`] over the same
//! window). The store keeps the result among its shared blocks, where a
//! fetch finds it after its pack record ([`super::Translations::prepared`])
//! and installs it after the same checks as any shared block (key, PC,
//! context, admitted bytes). The first PE that installs one offers it, so
//! the pack keeps only blocks that ran.
//!
//! A job decides only which block is prepared, never its code: a prepared
//! block is the block the engine compiles at that PC from those bytes in
//! that context. Successors of prepared blocks are queued in turn, up to
//! [`DEPTH`] steps from a fetched block.
use super::prepare::{Preparation, Prepared, window};
use super::stored::{Emission, Offer};
use super::{FETCH_PAGE, MAX_BLOCK_BYTES};
use crate::translations::{Digest, PAGE_BYTES};
use alloc::{boxed::Box, collections::VecDeque, sync::Arc, vec};

/// Static successor steps from a fetched block a job may lie.
pub const DEPTH: u8 = 3;
/// Jobs a queue holds; when full, the oldest go.
pub const CAPACITY: usize = 4096;
/// Slots of the queue's filter of recent jobs.
const SEEN: usize = 1 << 14;

/// A block to prepare: the page key, guest page and frame of its fetch page,
/// its entry PC, and the context and preparation of the block that names it.
#[derive(Debug, Clone)]
pub struct Job {
    key: Digest,
    page: u64,
    frame: u64,
    pc: u64,
    context: Digest,
    preparation: Arc<Preparation>,
    depth: u8,
    /// The naming block's [`Offer::quiet`].
    quiet: bool,
}

impl Job {
    pub fn key(&self) -> &Digest {
        &self.key
    }
    pub fn frame(&self) -> u64 {
        self.frame
    }
    pub fn pc(&self) -> u64 {
        self.pc
    }
    pub fn context(&self) -> &Digest {
        &self.context
    }
    /// Its entry PC's offset in its fetch page.
    pub fn offset(&self) -> u16 {
        (self.pc - self.page) as u16
    }
    fn tag(&self) -> u64 {
        let mut tag = u64::from_le_bytes(self.key.0[..8].try_into().expect("8 bytes"));
        tag ^= u64::from_le_bytes(self.context.0[..8].try_into().expect("8 bytes")).rotate_left(17);
        tag ^= self.pc.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        tag | 1
    }
}

impl Offer {
    /// Jobs for its static successors in its own fetch page, other than its
    /// own entry, while it lies fewer than [`DEPTH`] steps from a fetched
    /// block. A store with no frame (an offer made off any fetch) has none.
    pub fn ahead_jobs(&self) -> impl Iterator<Item = Job> + '_ {
        let open = self.depth < DEPTH && self.frame != 0;
        self.successors
            .iter()
            .copied()
            .filter(move |&target| {
                open && target != self.pc
                    && target & !(FETCH_PAGE - 1) == self.page
                    && target.is_multiple_of(4)
            })
            .map(|pc| Job {
                key: self.key,
                page: self.page,
                frame: self.frame,
                pc,
                context: self.context,
                preparation: self.emission.preparation.clone(),
                depth: self.depth + 1,
                quiet: self.quiet,
            })
    }
}

/// What became of the jobs a queue took and handed out
/// ([`Queue::note`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Jobs taken in.
    pub queued: u64,
    /// Jobs left out: the same block was queued recently.
    pub repeated: u64,
    /// Jobs that went unprepared because the queue was full.
    pub dropped: u64,
    /// Blocks prepared and kept for the PEs.
    pub prepared: u64,
    /// Jobs whose block the store held already (pack or shared).
    pub present: u64,
    /// Jobs whose frame no longer held the page's bytes.
    pub stale: u64,
    /// Jobs whose window prepares no block (a refused word, an access
    /// check, FP trapped).
    pub refused: u64,
}

/// How one job ended, for [`Counts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Prepared,
    Present,
    Stale,
    Refused,
}

/// Jobs waiting for a PE, newest first, at most [`CAPACITY`].
pub struct Queue {
    jobs: VecDeque<Job>,
    /// Tags of recent jobs, direct mapped: a block queued again while its
    /// tag is here is left out.
    seen: Box<[u64]>,
    counts: Counts,
}

impl Default for Queue {
    fn default() -> Self {
        Self::new()
    }
}

impl Queue {
    pub fn new() -> Self {
        Self {
            jobs: VecDeque::with_capacity(CAPACITY),
            seen: vec![0; SEEN].into_boxed_slice(),
            counts: Counts::default(),
        }
    }

    /// Take `job` in; the oldest goes when the queue is full. Allocates
    /// nothing.
    pub fn push(&mut self, job: Job) {
        let tag = job.tag();
        let slot = (tag as usize >> 1) % SEEN;
        if self.seen[slot] == tag {
            self.counts.repeated += 1;
            return;
        }
        self.seen[slot] = tag;
        if self.jobs.len() == CAPACITY {
            self.jobs.pop_front();
            self.counts.dropped += 1;
        }
        self.jobs.push_back(job);
        self.counts.queued += 1;
    }

    /// The newest job.
    pub fn pop(&mut self) -> Option<Job> {
        self.jobs.pop_back()
    }

    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    pub fn note(&mut self, outcome: Outcome) {
        let counter = match outcome {
            Outcome::Prepared => &mut self.counts.prepared,
            Outcome::Present => &mut self.counts.present,
            Outcome::Stale => &mut self.counts.stale,
            Outcome::Refused => &mut self.counts.refused,
        };
        *counter += 1;
    }

    pub fn counts(&self) -> Counts {
        self.counts
    }
}

/// Whether `page`, the bytes of the job's frame as read now, are the bytes
/// its key names.
pub fn page_matches(job: &Job, page: &[u8; PAGE_BYTES]) -> bool {
    Digest::of(page) == job.key
}

/// The block `job` names, prepared from `page`, the bytes of its frame that
/// hash to its key ([`page_matches`]): what a fetch of its PC in its context
/// compiles from them, as an offer of a block prepared ahead whose
/// successors are the next jobs.
pub fn prepare(job: &Job, page: &[u8; PAGE_BYTES]) -> Result<Offer, Outcome> {
    let offset = usize::from(job.offset());
    if job.preparation.context(job.page) != job.context {
        return Err(Outcome::Refused);
    }
    let length = window(job.pc);
    let mut staging = [0u8; MAX_BLOCK_BYTES as usize];
    let source = &mut staging[..length];
    source.copy_from_slice(&page[offset..offset + length]);
    job.preparation.admit(source);
    match job.preparation.prepare(source, job.pc, length) {
        Ok(Prepared::Block {
            compiled,
            input,
            successors,
            ..
        }) => Ok(Offer {
            key: job.key,
            page: job.page,
            pc: job.pc,
            context: job.context,
            emission: Arc::new(Emission::new(
                source[..input].to_vec(),
                compiled,
                job.preparation.clone(),
            )),
            successors,
            quiet: job.quiet,
            frame: job.frame,
            depth: job.depth,
        }),
        Ok(Prepared::Checked(_) | Prepared::FloatingPointTrap) | Err(_) => Err(Outcome::Refused),
    }
}

#[cfg(test)]
mod tests;
