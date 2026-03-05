use std::sync::atomic::{AtomicPtr, Ordering};
use std::cell::UnsafeCell;
use std::ptr;
use std::thread;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

struct QueueNode {
    thread: thread::Thread,
    next: AtomicPtr<QueueNode>,
}

impl QueueNode {
    fn new() -> Self {
        QueueNode {
            thread: thread::current(),
            next: AtomicPtr::new(ptr::null_mut()),
        }
    }
}

pub struct QueueBasedLock<T> {
    state: AtomicPtr<QueueNode>,
    data: UnsafeCell<T>,
}

const LOCKED: usize = 0b01;
const QUEUE_LOCKED: usize = 0b10;
const PTR_MASK: usize = !0b11;

impl<T> QueueBasedLock<T> {
    pub fn new(data: T) -> Self {
        QueueBasedLock {
            state: AtomicPtr::new(ptr::null_mut()),
            data: UnsafeCell::new(data),
        }
    }

    fn get_ptr(state: *mut QueueNode) -> *mut QueueNode {
        ((state as usize) & PTR_MASK) as *mut QueueNode
    }

    fn get_flags(state: *mut QueueNode) -> usize {
        (state as usize) & !PTR_MASK
    }

    fn make_state(ptr: *mut QueueNode, flags: usize) -> *mut QueueNode {
        ((ptr as usize) | flags) as *mut QueueNode
    }

    pub fn lock(&self) -> QueueBasedLockGuard<'_, T> {
        let state = self.state.load(Ordering::Relaxed);

        if Self::get_flags(state) & LOCKED == 0 {
            if self.state.compare_exchange(
                state,
                Self::make_state(Self::get_ptr(state), LOCKED),
                Ordering::Acquire,
                Ordering::Relaxed,
            ).is_ok() {
                return QueueBasedLockGuard { lock: self };
            }
        }

        let node = Box::into_raw(Box::new(QueueNode::new()));

        loop {
            let state = self.state.load(Ordering::Relaxed);
            let flags = Self::get_flags(state);
            let ptr = Self::get_ptr(state);

            if flags & LOCKED == 0 {
                match self.state.compare_exchange(
                    state,
                    Self::make_state(ptr, flags | LOCKED),
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        unsafe { drop(Box::from_raw(node)) };
                        return QueueBasedLockGuard { lock: self };
                    }
                    Err(_) => continue,
                }
            }

            if flags & QUEUE_LOCKED != 0 {
                std::hint::spin_loop();
                continue;
            }

            match self.state.compare_exchange(
                state,
                Self::make_state(ptr, flags | QUEUE_LOCKED),
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    unsafe {
                        if ptr.is_null() {
                            (*node).next.store(ptr::null_mut(), Ordering::Relaxed);
                        } else {
                            let mut tail = ptr;
                            while !(*tail).next.load(Ordering::Relaxed).is_null() {
                                tail = (*tail).next.load(Ordering::Relaxed);
                            }
                            (*tail).next.store(node, Ordering::Relaxed);
                        }

                        let new_ptr = if ptr.is_null() { node } else { ptr };
                        self.state.store(
                            Self::make_state(new_ptr, LOCKED),
                            Ordering::Release,
                        );
                    }

                    thread::park();
                    return QueueBasedLockGuard { lock: self };
                }
                Err(_) => continue,
            }
        }
    }

    fn unlock(&self) {
        loop {
            let state = self.state.load(Ordering::Relaxed);
            let flags = Self::get_flags(state);
            let ptr = Self::get_ptr(state);

            if ptr.is_null() {
                match self.state.compare_exchange(
                    state,
                    Self::make_state(ptr::null_mut(), 0),
                    Ordering::Release,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return,
                    Err(_) => continue,
                }
            }

            if flags & QUEUE_LOCKED != 0 {
                std::hint::spin_loop();
                continue;
            }

            match self.state.compare_exchange(
                state,
                Self::make_state(ptr, flags | QUEUE_LOCKED),
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    unsafe {
                        let next = (*ptr).next.load(Ordering::Relaxed);
                        let thread_to_wake = (*ptr).thread.clone();

                        drop(Box::from_raw(ptr));

                        self.state.store(
                            Self::make_state(next, LOCKED),
                            Ordering::Release,
                        );

                        thread_to_wake.unpark();
                    }
                    return;
                }
                Err(_) => continue,
            }
        }
    }
}

unsafe impl<T: Send> Send for QueueBasedLock<T> {}
unsafe impl<T: Send> Sync for QueueBasedLock<T> {}

pub struct QueueBasedLockGuard<'a, T> {
    lock: &'a QueueBasedLock<T>,
}

impl<'a, T> std::ops::Deref for QueueBasedLockGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<'a, T> std::ops::DerefMut for QueueBasedLockGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<'a, T> Drop for QueueBasedLockGuard<'a, T> {
    fn drop(&mut self) {
        self.lock.unlock();
    }
}

#[derive(Debug, Clone)]
struct Order {
    id: u64,
    symbol: String,
    quantity: u32,
    price: f64,
    timestamp: u128,
}

impl Order {
    fn new(id: u64, symbol: String, quantity: u32, price: f64) -> Self {
        Order {
            id,
            symbol,
            quantity,
            price,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        }
    }
}

struct OrderBook {
    orders: Vec<Order>,
    total_volume: u64,
}

impl OrderBook {
    fn new() -> Self {
        OrderBook {
            orders: Vec::new(),
            total_volume: 0,
        }
    }

    fn add_order(&mut self, order: Order) {
        self.total_volume += order.quantity as u64;
        self.orders.push(order);
    }

    fn get_stats(&self) -> (usize, u64) {
        (self.orders.len(), self.total_volume)
    }
}

fn main() {
    println!("=== Trading System with Queue-Based Lock ===\n");

    let order_book = Arc::new(QueueBasedLock::new(OrderBook::new()));
    let mut handles = vec![];

    let symbols = vec!["AAPL", "GOOGL", "MSFT", "TSLA"];

    for trader_id in 0..4 {
        let order_book = Arc::clone(&order_book);
        let symbol = symbols[trader_id].to_string();

        let handle = thread::spawn(move || {
            for order_id in 0..25 {
                let order = Order::new(
                    trader_id as u64 * 100 + order_id,
                    symbol.clone(),
                    (trader_id + 1) as u32 * 10,
                    150.0 + (order_id as f64 * 0.5),
                );

                let mut book = order_book.lock();
                book.add_order(order.clone());

                println!(
                    "[Trader {}] Order #{} placed: {} {} shares @ ${:.2}",
                    trader_id, order.id, order.symbol, order.quantity, order.price
                );

                thread::sleep(std::time::Duration::from_millis(5));
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    let book = order_book.lock();
    let (total_orders, total_volume) = book.get_stats();

    println!("\n=== Trading Session Complete ===");
    println!("Total Orders: {}", total_orders);
    println!("Total Volume: {} shares", total_volume);
    println!("Expected: 100 orders, 2500 shares");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_lock_unlock() {
        let lock = QueueBasedLock::new(0);
        {
            let mut guard = lock.lock();
            *guard = 42;
            assert_eq!(*guard, 42);
        }
        {
            let guard = lock.lock();
            assert_eq!(*guard, 42);
        }
    }

    #[test]
    fn test_multiple_threads_contending() {
        let lock = Arc::new(QueueBasedLock::new(0));
        let mut handles = vec![];

        for _ in 0..10 {
            let lock = Arc::clone(&lock);
            let handle = thread::spawn(move || {
                for _ in 0..100 {
                    let mut guard = lock.lock();
                    *guard += 1;
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let final_value = *lock.lock();
        assert_eq!(final_value, 1000);
    }

    #[test]
    fn test_fifo_fairness() {
        let lock = Arc::new(QueueBasedLock::new(Vec::new()));
        let mut handles = vec![];

        for i in 0..5 {
            let lock = Arc::clone(&lock);
            let handle = thread::spawn(move || {
                thread::sleep(std::time::Duration::from_millis(10 * i));
                let mut guard = lock.lock();
                guard.push(i);
                thread::sleep(std::time::Duration::from_millis(50));
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let order = lock.lock();
        assert_eq!(*order, vec![0, 1, 2, 3, 4]);
    }
}
