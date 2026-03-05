use std::sync::atomic::{
    AtomicPtr,
    AtomicUsize,
    Ordering,
};
use std::sync::Arc;
use std::ptr;
use std::thread::{self, Thread};
use std::mem;

// Constants for the state bits
const LOCKED_BIT: usize = 1 << 0;  // Bit 0: mutex is locked
const QUEUE_LOCK_BIT: usize = 1 << 1;  // Bit 1: queue operations are locked
const PTR_MASK: usize = !(LOCKED_BIT | QUEUE_LOCK_BIT);

/// A node in the waiting queue
struct WaitNode {
    thread: Thread,
    next: AtomicPtr<WaitNode>,
}

impl WaitNode {
    fn new() -> *mut WaitNode {
        Box::into_raw(Box::new(WaitNode {
            thread: thread::current(),
            next: AtomicPtr::new(ptr::null_mut()),
        }))
    }

    unsafe fn from_raw(ptr: *mut WaitNode) -> Box<WaitNode> {
        Box::from_raw(ptr)
    }
}

/// A queue-based mutex implementation
pub struct QueueMutex {
    // Atomic pointer combining state bits and queue head pointer
    state: AtomicUsize,
}

impl QueueMutex {
    /// Create a new unlocked mutex
    pub fn new() -> Self {
        QueueMutex {
            state: AtomicUsize::new(0),
        }
    }

    /// Try to acquire the lock without blocking
    pub fn try_lock(&self) -> bool {
        let mut state = self.state.load(Ordering::Acquire);

        loop {
            // If already locked, fail
            if state & LOCKED_BIT != 0 {
                return false;
            }

            // Try to set the locked bit
            let new_state = state | LOCKED_BIT;
            match self.state.compare_exchange_weak(
                state,
                new_state,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(actual) => state = actual,
            }
        }
    }

    /// Acquire the lock, blocking if necessary
    pub fn lock(&self) {
        let mut state = self.state.load(Ordering::Acquire);

        loop {
            // Fast path: try to acquire if unlocked
            if state & LOCKED_BIT == 0 {
                let new_state = state | LOCKED_BIT;
                match self.state.compare_exchange_weak(
                    state,
                    new_state,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(actual) => {
                        state = actual;
                        continue;
                    }
                }
            }

            // Slow path: need to enqueue ourselves
            self.enqueue_and_wait();
            state = self.state.load(Ordering::Acquire);
        }
    }

    /// Release the lock
    pub fn unlock(&self) {
        let mut state = self.state.load(Ordering::Acquire);

        loop {
            // Must be locked
            debug_assert!(state & LOCKED_BIT != 0);

            let new_state = state & !LOCKED_BIT;

            match self.state.compare_exchange_weak(
                state,
                new_state,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    // If there's a queue, wake the first waiter
                    if state & PTR_MASK != 0 {
                        self.wake_next();
                    }
                    return;
                }
                Err(actual) => state = actual,
            }
        }
    }

    /// Enqueue current thread and wait
    fn enqueue_and_wait(&self) {
        // Create a wait node for this thread
        let node = WaitNode::new();
        let node_ptr = node as usize;

        let mut state = self.state.load(Ordering::Acquire);

        loop {
            // Acquire queue lock
            if state & QUEUE_LOCK_BIT != 0 {
                // Queue is being modified, spin briefly
                for _ in 0..10 {
                    std::hint::spin_loop();
                }
                state = self.state.load(Ordering::Acquire);
                continue;
            }

            // Try to acquire queue lock
            let new_state = state | QUEUE_LOCK_BIT;
            match self.state.compare_exchange_weak(
                state,
                new_state,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => state = actual,
            }
        }

        // Now we have the queue lock, add ourselves
        let queue_head = (state & PTR_MASK) as *mut WaitNode;

        unsafe {
            (*node).next.store(queue_head, Ordering::Release);
        }

        // Update state with new queue head and release queue lock
        let new_state = (state & QUEUE_LOCK_BIT) | node_ptr;
        self.state.store(new_state, Ordering::Release);

        // Park this thread until unparked
        thread::park();

        // Clean up our node
        unsafe {
            let _ = WaitNode::from_raw(node);
        }
    }

    /// Wake the next waiting thread
    fn wake_next(&self) {
        let mut state = self.state.load(Ordering::Acquire);

        loop {
            // Acquire queue lock
            if state & QUEUE_LOCK_BIT != 0 {
                for _ in 0..10 {
                    std::hint::spin_loop();
                }
                state = self.state.load(Ordering::Acquire);
                continue;
            }

            let new_state = state | QUEUE_LOCK_BIT;
            match self.state.compare_exchange_weak(
                state,
                new_state,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => state = actual,
            }
        }

        // Get and remove the head of the queue
        let head_ptr = (state & PTR_MASK) as *mut WaitNode;

        let next_ptr = unsafe {
            if head_ptr.is_null() {
                ptr::null_mut()
            } else {
                (*head_ptr).next.load(Ordering::Acquire)
            }
        };

        // Update state with new head and release queue lock
        let new_state = (state & LOCKED_BIT) | (next_ptr as usize);
        self.state.store(new_state, Ordering::Release);

        // Wake the thread
        if !head_ptr.is_null() {
            unsafe {
                let thread = (*head_ptr).thread.clone();
                thread.unpark();
            }
        }
    }
}

impl Default for QueueMutex {
    fn default() -> Self {
        Self::new()
    }
}

unsafe impl Send for QueueMutex {}
unsafe impl Sync for QueueMutex {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_basic_lock() {
        let mutex = QueueMutex::new();

        mutex.lock();
        mutex.unlock();

        assert!(mutex.try_lock());
        mutex.unlock();
    }

    #[test]
    fn test_try_lock_fails_when_locked() {
        let mutex = QueueMutex::new();

        mutex.lock();
        assert!(!mutex.try_lock());
        mutex.unlock();
    }

    #[test]
    fn test_concurrent_access() {
        let mutex = Arc::new(QueueMutex::new());
        let counter = Arc::new(AtomicUsize::new(0));

        let mut handles = vec![];

        for _ in 0..10 {
            let mutex_clone = Arc::clone(&mutex);
            let counter_clone = Arc::clone(&counter);

            let handle = thread::spawn(move || {
                for _ in 0..1000 {
                    mutex_clone.lock();
                    counter_clone.fetch_add(1, Ordering::Relaxed);
                    mutex_clone.unlock();
                }
            });

            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(counter.load(Ordering::Relaxed), 10000);
    }
}