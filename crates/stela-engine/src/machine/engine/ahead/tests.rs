use super::super::stored::tests::MemoryStore;
use super::super::tests::{BASE, machine};
use super::super::{Fetched, Host};
use super::*;
use alloc::vec::Vec;

/// `cbz x0, +12; add x1, x1, #1; add x2, x2, #2; add x3, x3, #3; svc #0`:
/// one block, whose side exit leaves for the third ADD.
const WORDS: [u32; 5] = [
    0xb400_0060,
    0x9100_0421,
    0x9100_0842,
    0x9100_0c63,
    0xd400_0001,
];

/// The offer the fetch of `pc` makes in a machine with a store.
fn fetched_offer(pc: u64) -> Offer {
    let mut fetching = machine(&WORDS);
    fetching.host.store = Some(MemoryStore::default());
    assert!(matches!(
        fetching.fetch_block(pc, false),
        Ok(Fetched::Block(_))
    ));
    let store = fetching.host.store.as_mut().unwrap();
    assert_eq!(store.offers.len(), 1);
    store.offers.pop().unwrap()
}

/// The page holding `BASE`, as the test machine's RAM holds it.
fn page() -> Box<[u8; PAGE_BYTES]> {
    let mut reader = machine(&WORDS);
    let mut page = Box::new([0u8; PAGE_BYTES]);
    assert!(
        reader
            .host
            .read_ram(BASE & !(FETCH_PAGE - 1), page.as_mut_slice())
    );
    page
}

#[test]
fn a_fetched_block_names_its_successors_in_its_page_as_jobs() {
    let offer = fetched_offer(BASE);
    let mut targets: Vec<u64> = offer.ahead_jobs().map(|job| job.pc()).collect();
    targets.sort_unstable();
    assert_eq!(targets, [BASE + 12]);
    for job in offer.ahead_jobs() {
        assert_eq!(job.key(), &offer.key());
        assert_eq!(job.context(), &offer.context());
        assert_eq!(job.frame(), BASE & !(FETCH_PAGE - 1));
    }
}

#[test]
fn a_block_prepared_ahead_is_the_block_a_fetch_compiles() {
    let offer = fetched_offer(BASE);
    let page = page();
    for job in offer.ahead_jobs() {
        assert!(page_matches(&job, &page));
        let prepared = prepare(&job, &page).expect("a block");
        assert!(prepared.ahead());
        assert_eq!(prepared.pc(), job.pc());
        // The same PC fetched in a machine of its own compiles it.
        let compiled = fetched_offer(job.pc());
        assert!(!compiled.ahead());
        assert_eq!(prepared.input(), compiled.input());
        assert_eq!(prepared.context(), compiled.context());
        assert_eq!(
            prepared.block().native.x86_64_code,
            compiled.block().native.x86_64_code
        );
        assert_eq!(
            prepared.block().native.source_map,
            compiled.block().native.source_map
        );
        assert_eq!(prepared.successors(), compiled.successors());
    }
}

#[test]
fn a_fetch_installs_a_block_prepared_ahead_and_offers_it_for_the_pack_once() {
    let offer = fetched_offer(BASE);
    let page = page();
    let job = offer
        .ahead_jobs()
        .find(|job| job.pc() == BASE + 12)
        .expect("the CBZ's target");
    let prepared = prepare(&job, &page).expect("a block");
    let code = prepared.block().native.x86_64_code.clone();
    let mut store = MemoryStore::default();
    store.prepared.insert(
        (prepared.key(), prepared.pc(), prepared.context()),
        prepared.shared(),
    );
    let mut receiver = machine(&WORDS);
    receiver.host.store = Some(store);
    assert!(matches!(
        receiver.fetch_block(BASE + 12, false),
        Ok(Fetched::Block(_))
    ));
    let level = usize::from(receiver.cpu.pstate.el != 0);
    assert_eq!(
        (receiver.statistics.shared, receiver.statistics.compiled),
        (1, 0)
    );
    assert_eq!(receiver.statistics.ahead_installs[level], 1);
    assert_eq!(receiver.host.installed.first(), Some(&code));
    // Offered once, as a block that ran, with no successors to queue.
    let store = receiver.host.store.as_mut().unwrap();
    assert_eq!(store.offers.len(), 1);
    let kept = &store.offers[0];
    assert!(!kept.ahead());
    assert_eq!(kept.pc(), BASE + 12);
    assert_eq!(kept.ahead_jobs().count(), 0);
    assert!(kept.entry().is_some());
}

#[test]
fn a_frame_that_changed_is_stale() {
    let offer = fetched_offer(BASE);
    let mut page = page();
    let job = offer.ahead_jobs().next().expect("a job");
    page[(job.pc() - BASE) as usize] ^= 1;
    assert!(!page_matches(&job, &page));
}

#[test]
fn jobs_stop_at_the_depth_bound() {
    let offer = fetched_offer(BASE);
    let page = page();
    let mut job = offer
        .ahead_jobs()
        .find(|job| job.pc() == BASE + 12)
        .expect("the side exit's target");
    // Every depth to the bound, by hand.
    for depth in 1..=DEPTH {
        job.depth = depth;
        let prepared = prepare(&job, &page).expect("a block");
        let successors = prepared
            .successors()
            .iter()
            .filter(|&&pc| pc != job.pc())
            .count();
        let jobs = prepared.ahead_jobs().count();
        if depth < DEPTH {
            assert_eq!(jobs, successors);
        } else {
            assert_eq!(jobs, 0);
        }
    }
}

#[test]
fn a_queue_hands_out_the_newest_job_skips_repeats_and_drops_the_oldest() {
    let offer = fetched_offer(BASE);
    let template = offer.ahead_jobs().next().expect("a job");
    let job = |pc: u64| Job {
        pc,
        ..template.clone()
    };
    let mut queue = Queue::new();
    queue.push(job(BASE + 4));
    queue.push(job(BASE + 8));
    queue.push(job(BASE + 8));
    assert_eq!(queue.counts().queued, 2);
    assert_eq!(queue.counts().repeated, 1);
    assert_eq!(queue.pop().map(|job| job.pc()), Some(BASE + 8));
    assert_eq!(queue.pop().map(|job| job.pc()), Some(BASE + 4));
    assert!(queue.pop().is_none());
    for index in 0..CAPACITY as u64 + 3 {
        queue.push(job(BASE + 0x10_0000 + index * 4));
    }
    assert_eq!(queue.len(), CAPACITY);
    assert_eq!(queue.counts().dropped, 3);
    assert_eq!(
        queue.pop().map(|job| job.pc()),
        Some(BASE + 0x10_0000 + (CAPACITY as u64 + 2) * 4)
    );
}
