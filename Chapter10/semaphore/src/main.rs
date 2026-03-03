use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, Condvar};
use std::thread;
use std::time::{Duration, Instant};

pub struct SemaphoreSpinLock {
    value: AtomicU8,
}

impl SemaphoreSpinLock {
    pub fn new(initial_value: u8) -> Self {
        SemaphoreSpinLock {
            value: AtomicU8::new(initial_value)
        }
    }

    pub fn signal(&self) {
        self.value.fetch_add(1, Ordering::SeqCst);
    }
    pub fn value(&self)-> u8 {
        return self.value.load(Ordering::SeqCst);
    }

    pub fn wait(&self) {
        // spin lock
        loop {
            let current = self.value.load(Ordering::SeqCst);
            if current > 0 {
                match self.value.compare_exchange(current, current - 1, Ordering::SeqCst, Ordering::SeqCst)
                {
                    Ok(_) => break,
                    Err(_) => continue,
                }
            }
        }
    }
}

pub struct SemaphoreCondVar {
    value: AtomicU8,
    condvar: Condvar,
    mutex: Mutex<()>
}

impl SemaphoreCondVar{
    pub fn new(initial_value: u8) -> Self {
        SemaphoreCondVar {
            value: AtomicU8::new(initial_value),
            condvar: Condvar::new(),
            mutex: Mutex::new(()),
        }
    }

    pub fn value(&self)-> u8 {
        return self.value.load(Ordering::SeqCst);
    }

    pub fn signal(&self) {
        self.value.fetch_add(1, Ordering::SeqCst);
        self.condvar.notify_one();
    }

    pub fn wait(&self) {
        let mut guard = self.mutex.lock().unwrap();
        loop {
            let current = self.value.load(Ordering::SeqCst);
            if current > 0 {
                match self.value.compare_exchange(current, current - 1, Ordering::SeqCst, Ordering::SeqCst) {
                    Ok(_) => break,
                    Err(_) => continue,
                }
            } else {
                guard = self.condvar.wait(guard).unwrap();
            }
        }
    }

    pub fn wait_simple(&self){
        let mut guard = self.mutex.lock().unwrap();

        while self.value.load(Ordering::SeqCst) == 0{
            guard = self.condvar.wait(guard).unwrap();
        }

        self.value.fetch_sub(1, Ordering::SeqCst);
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signal_and_wait() {
        let sem = SemaphoreSpinLock::new(2);
        assert_eq!(sem.value(), 2);

        // decrement twice
        sem.wait();
        assert_eq!(sem.value(), 1);
        sem.wait();
        assert_eq!(sem.value(), 0);

        // increment once
        sem.signal();
        assert_eq!(sem.value(), 1);

        // decrement again
        sem.wait();
        assert_eq!(sem.value(), 0);

        // testing condvar version
        let sem = SemaphoreCondVar::new(2);
        assert_eq!(sem.value(), 2);

        // decrement twice
        sem.wait();
        assert_eq!(sem.value(), 1);
        sem.wait();
        assert_eq!(sem.value(), 0);

        // increment once
        sem.signal();
        assert_eq!(sem.value(), 1);

        // decrement again
        sem.wait();
        assert_eq!(sem.value(), 0);
    }

    #[test]
    fn test_multithread() {
        let sem = Arc::new(SemaphoreSpinLock::new(2));
        let mut handles = vec![];

        for i in 0..4 {
            let sem_clone = Arc::clone(&sem);
            let handle = thread::spawn(move ||
                {
                    println!("Thread {} waiting", i);
                    sem_clone.wait();
                    println!("Thread {} acquired", i);

                    thread::sleep(std::time::Duration::from_millis(100));

                    sem_clone.signal();
                    println!("Thread {} released", i);
                });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(sem.value(), 2);


        let sem = Arc::new(SemaphoreCondVar::new(2));
        let mut handles = vec![];

        for i in 0..4 {
            let sem_clone = Arc::clone(&sem);
            let handle = thread::spawn(move ||
                {
                    println!("Thread {} waiting", i);
                    sem_clone.wait();
                    println!("Thread {} acquired", i);

                    thread::sleep(std::time::Duration::from_millis(100));

                    sem_clone.signal();
                    println!("Thread {} released", i);
                });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(sem.value(), 2);
    }
}

#[derive(Clone, Debug)]
enum Side {
    Buy,
    Sell,
}

struct TradeOrder {
    id: u32,
    symbol: String,
    quantity: u32,
    side: Side,
}

fn main() {
    let api_limit = Arc::new(SemaphoreCondVar::new(4));

    println!("Trading Bot: Max 4 concurrent API Calls");

    let orders = vec![
        TradeOrder {id: 1, symbol: "AAPL".to_string(), quantity: 5, side: Side::Buy},
        TradeOrder {id: 2, symbol: "IBM".to_string(), quantity: 5, side: Side::Buy},
        TradeOrder {id: 3, symbol: "ABC".to_string(), quantity: 5, side: Side::Buy},
        TradeOrder {id: 4, symbol: "NVDA".to_string(), quantity: 5, side: Side::Buy},
        TradeOrder {id: 5, symbol: "QQQ".to_string(), quantity: 5, side: Side::Buy},
        TradeOrder {id: 6, symbol: "PAEL".to_string(), quantity: 5, side: Side::Sell},
        TradeOrder {id: 7, symbol: "EFERT".to_string(), quantity: 5, side: Side::Buy},
        TradeOrder {id: 8, symbol: "A".to_string(), quantity: 5, side: Side::Sell},
        TradeOrder {id: 9, symbol: "MZN".to_string(), quantity: 5, side: Side::Buy},
        TradeOrder {id: 10, symbol: "MIIETF".to_string(), quantity: 5, side: Side::Sell},
    ];

    let mut handles = vec![];

    for order in orders {
        let sem_clone = Arc::clone(&api_limit);

        let handle = thread::spawn(move || {
            println!("Waiting to place Order ID: {} {} {}", order.id, order.quantity, order.symbol);
            sem_clone.wait_simple();

            let start = Instant::now();

            match order.side {
                Side::Sell => println!("Sell Order"),
                Side::Buy => println!("Buy Order"),
            }

            println!("Placing Orders through API {}", order.id);

            thread::sleep(Duration::from_millis(100));

            println!("Order TIme Taken: {:?}", start.elapsed());

            sem_clone.signal();

        });

        handles.push(handle);
    }

    for handle in handles{
        handle.join().unwrap();
    }

    println!("All Orders Processed");

}
