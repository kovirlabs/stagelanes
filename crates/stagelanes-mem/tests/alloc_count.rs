//! The counting allocator installed as the global allocator: an armed
//! thread's allocations are counted, other threads' are not.

use stagelanes_mem::CountingAlloc;
use stagelanes_mem::alloc_count::{
    arm_current_thread, current_thread_count, disarm_current_thread,
};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

#[test]
fn counts_armed_threads_only() {
    std::thread::spawn(|| {
        arm_current_thread();
        let mut v: Vec<u64> = Vec::with_capacity(4); // 1 allocation
        v.extend([1, 2, 3, 4]);
        v.push(5); // grows: 1 reallocation
        let boxed = Box::new(7u32); // 1 allocation
        let n = disarm_current_thread();
        assert!(n >= 3, "expected at least 3 counted allocations, got {n}");
        drop((v, boxed));
        // Disarmed: further allocations are not counted.
        let more: Vec<u8> = std::hint::black_box(Vec::with_capacity(64));
        assert!(more.capacity() >= 64);
        assert_eq!(current_thread_count(), n);
    })
    .join()
    .expect("armed thread");

    std::thread::spawn(|| {
        let v = vec![0u8; 1024];
        assert_eq!(v.len(), 1024);
        assert_eq!(
            current_thread_count(),
            0,
            "a thread that never armed counts nothing"
        );
    })
    .join()
    .expect("unarmed thread");
}

#[test]
fn no_allocation_is_counted_as_zero() {
    std::thread::spawn(|| {
        let mut buf = [0u64; 32];
        arm_current_thread();
        for (i, x) in buf.iter_mut().enumerate() {
            *x = i as u64 * 3;
        }
        let n = disarm_current_thread();
        assert_eq!(n, 0);
        assert_eq!(buf[31], 93);
    })
    .join()
    .expect("thread");
}
