use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use super::*;

/// Counts heap allocations made by the current thread.
struct CountingAlloc;

thread_local! {
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: forwards to the system allocator unchanged; only adds a counter.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

fn allocs() -> usize {
    ALLOCS.with(Cell::get)
}

fn small(large: LargePages) -> RegionConfig {
    RegionConfig {
        large_pages: large,
        ..RegionConfig::new(256 << 10)
    }
}

#[test]
fn rejects_zero_and_oversize() {
    assert_eq!(
        Region::new(&RegionConfig::new(0)).unwrap_err(),
        Error::ZeroSize
    );
    let cfg = RegionConfig {
        max_bytes: 1 << 20,
        ..RegionConfig::new(2 << 20)
    };
    assert!(matches!(Region::new(&cfg), Err(Error::OverCap { .. })));
}

#[test]
fn normal_pages_locked_zeroed_writable() {
    let mut r = Region::new(&small(LargePages::Off)).expect("locked normal pages");
    assert_eq!(r.pages(), PageKind::Normal);
    assert!(r.locked());
    assert!(r.len() >= 256 << 10);
    assert_eq!(r.len() % r.page_size(), 0);
    assert_eq!(r.as_ptr() as usize % r.page_size(), 0);
    assert!(r.as_slice().iter().all(|&b| b == 0));
    r.as_mut_slice().fill(0xAB);
    assert!(r.as_slice().iter().all(|&b| b == 0xAB));
}

#[test]
fn unlocked_region() {
    let cfg = RegionConfig {
        lock: false,
        ..small(LargePages::Off)
    };
    let r = Region::new(&cfg).unwrap();
    assert!(!r.locked());
}

#[test]
fn large_pages_prefer_falls_back() {
    let mut r = Region::new(&small(LargePages::Prefer))
        .expect("prefer never fails for lack of large pages");
    if r.pages() == PageKind::Large {
        assert!(r.page_size() >= 1 << 20);
    }
    r.as_mut_slice()[0] = 1;
}

#[test]
fn large_pages_require_succeeds_or_says_why() {
    match Region::new(&small(LargePages::Require)) {
        Ok(r) => {
            assert_eq!(r.pages(), PageKind::Large);
            assert!(r.locked());
        }
        Err(Error::LargePagesUnavailable(why)) => assert!(!why.is_empty()),
        Err(e) => panic!("unexpected error: {e}"),
    }
}

#[test]
fn numa_node_zero_and_missing_node() {
    let cfg = RegionConfig {
        numa_node: Some(0),
        ..small(LargePages::Off)
    };
    assert_eq!(Region::new(&cfg).unwrap().numa_node(), Some(0));
    let cfg = RegionConfig {
        numa_node: Some(4000),
        ..small(LargePages::Off)
    };
    assert_eq!(Region::new(&cfg).unwrap_err(), Error::NoSuchNode(4000));
}

#[test]
fn block_pool_hands_out_disjoint_aligned_blocks() {
    let region = Region::new(&small(LargePages::Off)).unwrap();
    let mut pool = BlockPool::new(region, 1000).unwrap();
    assert_eq!(pool.block_bytes(), 1024);
    let n = pool.capacity();
    assert_eq!(n, pool.region_info().len() / 1024);
    let mut blocks: Vec<Block> = (0..n).map(|_| pool.take().unwrap()).collect();
    assert!(pool.take().is_none());
    for (i, b) in blocks.iter_mut().enumerate() {
        assert_eq!(b.as_ptr() as usize % 64, 0);
        b.as_mut_slice().fill(i as u8);
    }
    for (i, b) in blocks.iter().enumerate() {
        assert!(b.as_slice().iter().all(|&x| x == i as u8));
    }
    for b in blocks {
        pool.put(b).unwrap();
    }
    assert_eq!(pool.available(), n);
}

#[test]
fn block_pool_rejects_foreign_blocks() {
    let mut a = BlockPool::new(Region::new(&small(LargePages::Off)).unwrap(), 4096).unwrap();
    let mut b = BlockPool::new(Region::new(&small(LargePages::Off)).unwrap(), 4096).unwrap();
    let from_a = a.take().unwrap();
    let back = b.put(from_a).unwrap_err();
    a.put(back).unwrap();
}

#[test]
fn block_pool_take_and_put_do_not_allocate() {
    let mut pool = BlockPool::new(Region::new(&small(LargePages::Off)).unwrap(), 256).unwrap();
    let mut held = Vec::with_capacity(pool.capacity());
    let before = allocs();
    for _ in 0..10 {
        while let Some(b) = pool.take() {
            held.push(b);
        }
        while let Some(b) = held.pop() {
            pool.put(b).unwrap();
        }
    }
    assert_eq!(allocs(), before);
}

#[test]
fn blocks_cross_threads_and_outlive_their_pool() {
    let mut pool = BlockPool::new(Region::new(&small(LargePages::Off)).unwrap(), 4096).unwrap();
    let mut block = pool.take().unwrap();
    let block = std::thread::spawn(move || {
        block.as_mut_slice().fill(7);
        block
    })
    .join()
    .unwrap();
    drop(pool); // leaks the region because `block` is still out
    assert!(block.as_slice().iter().all(|&x| x == 7));
}

#[test]
fn block_bigger_than_region_is_an_error() {
    let region = Region::new(&small(LargePages::Off)).unwrap();
    let len = region.len();
    assert!(BlockPool::new(region, len + 1).is_err());
}
