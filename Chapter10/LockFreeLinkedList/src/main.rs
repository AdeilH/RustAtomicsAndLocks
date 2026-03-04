use std::ptr;
use std::sync::{Arc, atomic::AtomicPtr, atomic::AtomicUsize, atomic::Ordering};

struct Node<T> {
    data: T,
    next: AtomicPtr<Node<T>>,
    ref_count: AtomicUsize,
}

impl<T: Clone> Node<T> {
    pub fn new(data: T) -> Self {
        Self {
            data,
            next: AtomicPtr::new(ptr::null_mut()),
            ref_count: AtomicUsize::new(0),
        }
    }
}

struct LinkedList<T> {
    head: AtomicPtr<Node<T>>,
    ref_count: AtomicUsize,
}

impl<T: Clone> LinkedList<T> {
    pub fn new() -> Self {
        Self {
            head: AtomicPtr::new(ptr::null_mut()),
            ref_count: AtomicUsize::new(0),
        }
    }

    pub fn insert(&self, data: T) {
        let new_node = Box::into_raw(Box::new(Node::new(data)));
        loop {
            let head = self.head.load(Ordering::Acquire);
            unsafe {
                (*new_node).next.store(head, Ordering::Release);
            }
            if (self
                .head
                .compare_exchange_weak(head, new_node, Ordering::Release, Ordering::Relaxed)
                .is_ok())
            {
                break;
            }
        }
    }

    pub fn delete(&self, data: &T) -> bool
    where
        T: PartialEq,
    {
        let head = self.head.load(Ordering::Acquire);
        let mut curr = head;
        let mut prev: *mut AtomicPtr<Node<T>> = &self.head as *const _ as *mut _;

        while curr.is_null() != true {
            unsafe {
                if &(*curr).data == data {
                    let next = (*curr).next.load(Ordering::Acquire);
                    if ((*prev)
                        .compare_exchange_weak(curr, next, Ordering::Release, Ordering::Relaxed)
                        .is_ok())
                    {
                        self.defer_destroy_node(curr);
                        return true;
                    } else {
                        break;
                    }
                }

                prev = &mut (*curr).next;
                curr = (*curr).next.load(Ordering::Acquire);
            }
        }
        if (curr.is_null()) {
            return false;
        }
        return false;
    }

    pub fn contains(&self, data: &T) -> bool
    where
        T: PartialEq,
    {
        self.ref_count.fetch_add(1, Ordering::SeqCst);
        let mut curr = self.head.load(Ordering::Acquire);
        while (!curr.is_null()) {
            unsafe {
                if &(*curr).data == data {
                    self.ref_count.fetch_sub(1, Ordering::Release);
                    return true;
                }
                curr = (*curr).next.load(Ordering::Acquire);
            }
        }
        self.ref_count.fetch_sub(1, Ordering::Release);
        return false;
    }

    pub fn defer_destroy_node(&self, node: *mut Node<T>) {
        while (self.ref_count.load(Ordering::SeqCst) > 0) {
            std::hint::spin_loop()
        }
        unsafe {
            drop(Box::from_raw(node));
        }
    }
}

impl<T> Drop for LinkedList<T> {
    fn drop(&mut self) {
        let mut curr = self.head.load(Ordering::Acquire);
        while (!curr.is_null()) {
            unsafe {
                let next = (*curr).next.load(Ordering::Acquire);
                drop(Box::from_raw(curr));
                curr = next;
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct TradeEvent {
    symbol: String,
    price: u64,
    quantity: u32,
    timestamp: u128,
}

impl TradeEvent {
    pub fn new(symbol: String, price: u64, quantity: u32) -> Self {
        Self {
            symbol,
            price,
            quantity,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as u128,
        }
    }
}

fn get_last_n_events(list: &LinkedList<TradeEvent>, n: usize) -> Vec<TradeEvent> {
    let mut result = Vec::with_capacity(n);
    let mut curr = list.head.load(Ordering::Acquire);

    while (!curr.is_null() && result.len() < n) {
        unsafe {
            let data = (*curr).data.clone();
            result.push(data);
            curr = (*curr).next.load(Ordering::Acquire);
        }
    }
    result
}

fn count_recent_trades(list: &LinkedList<TradeEvent>, millisecond_time: u128) -> u32 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();

    let mut count: u32 = 0;
    let mut curr = list.head.load(Ordering::Acquire);

    while (!curr.is_null()) {
        unsafe {
            let ts = (*curr).data.timestamp;
            if (now - ts <= millisecond_time) {
                count = count + 1
            } else {
                break;
            }
            curr = (*curr).next.load(Ordering::Acquire);
        }
    }
    count
}

fn main() {
    let trade_list: LinkedList<TradeEvent> = LinkedList::new();
    let shared_list = Arc::new(trade_list);

    // === Writer Thread ===
    let writer = {
        let list = Arc::clone(&shared_list);
        std::thread::spawn(move || {
            for i in 1..=10 {
                let event = TradeEvent {
                    symbol: "TSLA".to_string(),
                    price: 200 + (i as u64) * 25,
                    quantity: 50 * i,
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as u128,
                };

                list.insert(event);
                println!("[TRADE ENGINE] Inserted trade #{}", i);
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
        })
    };

    // === Reader Thread 1: Dashboard UI ===
    let reader_ui = {
        let list = Arc::clone(&shared_list);
        std::thread::spawn(move || {
            loop {
                let events = get_last_n_events(&list, 3);
                println!("[DASHBOARD] Latest Trades: {:?}", events);
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        })
    };

    // === Reader Thread 2: Risk Engine ===
    let reader_risk = {
        let list = Arc::clone(&shared_list);
        std::thread::spawn(move || {
            loop {
                let count = count_recent_trades(&list, 10000); // last 1 sec
                println!("[RISK ENGINE] Trades in last 1s: {}", count);
                std::thread::sleep(std::time::Duration::from_millis(10000));
            }
        })
    };

    writer.join().unwrap();
    reader_ui.join().unwrap();
    reader_risk.join().unwrap();
}
