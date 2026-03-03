use std::ops::Deref;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

pub struct RCUData<T> {
    data: Mutex<Arc<T>>,
    reader_count: Arc<AtomicUsize>,
}

pub struct RCUReader<T> {
    data: Arc<T>,
    reader_count: Arc<AtomicUsize>,
}

impl<T: Clone> RCUData<T> {
    pub fn new(data: T) -> Self {
        Self {
            data: Mutex::new(Arc::new(data)),
            reader_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn read(&self) -> RCUReader<T> {
        self.reader_count.fetch_add(1, Ordering::SeqCst);

        RCUReader {
            data: Arc::clone(&*self.data.lock().unwrap()),
            reader_count: Arc::clone(&self.reader_count),
        }
    }

    pub fn write<F>(&self, f: F)
    where
        F: FnOnce(&mut T),
    {
        // lock to get write excess
        let guard = self.data.lock().unwrap();

        // Clone current data
        let mut new_data = (**guard).clone();

        // Unlock so readers can continue
        drop(guard);

        // modify the copy
        f(&mut new_data);

        // wait for old readers to finish
        while self.reader_count.load(Ordering::SeqCst) > 0 {
            std::thread::yield_now()
        }

        // Lock again and swap the data with new Version
        let mut guard = self.data.lock().unwrap();
        *guard = Arc::new(new_data);
    }

    pub fn safer_write<F: FnOnce(&mut T)>(&self, f: F) {
        let mut new_data = {
            let guard = self.data.lock().unwrap();
            (**guard).clone()
        };

        f(&mut new_data);

        while self.reader_count.load(Ordering::SeqCst) > 0 {
            std::thread::yield_now();
        }

        // Re-lock and swap (ensures we have latest)
        let mut guard = self.data.lock().unwrap();
        *guard = Arc::new(new_data);
    }
}

impl<T> Deref for RCUReader<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &*self.data
    }
}

impl<T> Drop for RCUReader<T> {
    fn drop(&mut self) {
        self.reader_count.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_single_reader() {
        let rcu = RCUData::new(vec![1, 2, 3]);
        let reader = rcu.read();
        assert_eq!(reader.reader_count.load(Ordering::SeqCst), 1);
        assert_eq!(*reader, vec![1, 2, 3]);
    }

    #[test]
    fn test_single_writer() {
        let rcu_data = RCUData::new(vec![3, 4, 5]);

        let modify_func = |data: &mut Vec<u32>| {
            data.push(6);
        };

        rcu_data.write(modify_func);

        let reader = rcu_data.read();

        assert_eq!(*reader, vec![3, 4, 5, 6])
    }

    #[test]
    fn test_multiple_reader() {
        let rcu_data = Arc::new(RCUData::new(123));
        let mut handles = vec![];

        for _ in 0..5 {
            let rcu_clone = Arc::clone(&rcu_data);
            let handle = thread::spawn(move || {
                let reader = rcu_clone.read();
                thread::sleep(std::time::Duration::from_millis(500));
                *reader
            });
            handles.push(handle);
        }

        for handle in handles {
            assert_eq!(handle.join().unwrap(), 123);
        }
    }

    #[test]
    fn test_writer_waits_for_readers() {
        let data = Arc::new(RCUData::new("abc".to_string()));
        let data_clone = Arc::clone(&data);

        let (tx, rx) = std::sync::mpsc::channel();

        let reader_thread = thread::spawn(move || {
            let reader = data_clone.read();
            assert_eq!(*reader, "abc".to_string());
            tx.send(()).unwrap();
            thread::sleep(std::time::Duration::from_millis(250));
        });

        rx.recv().unwrap();

        let change_func = |data_updated: &mut String| {
            *data_updated = "def".to_string()
        };

        let strat = std::time::Instant::now();

        data.write(change_func);

        let elapsed = strat.elapsed();
        assert_eq!(data.reader_count.load(Ordering::SeqCst), 0);

        reader_thread.join().unwrap();
        assert!(elapsed.as_micros() > 100);
        let reader = data.read();
        assert_eq!(data.reader_count.load(Ordering::SeqCst), 1);
        let def = "def".to_string();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(*reader, def);
    }

    #[test]
    fn test_new_readers_see_new_data() {
        let rcu = Arc::new(RCUData::new(33));

        let rcu_clone1 = Arc::clone(&rcu);
        let reader_thread1 = thread::spawn(move || {
            let reader = rcu_clone1.read();
            assert_eq!(*reader, 33);
            thread::sleep(std::time::Duration::from_millis(300));
        });

        thread::sleep(std::time::Duration::from_millis(250));

        rcu.write(|data| {
            *data = 42;
        });

        let reader2 = rcu.read();
        assert_eq!(*reader2, 42);

        reader_thread1.join().unwrap();
    }

    #[test]
    fn test_mutex_serializes_writers() {
        let rcu = Arc::new(RCUData::new(0));
        let mut handles = vec![];

        // let (tx, rx) = std::sync::mpsc::channel();

        for _ in 0..4 {
            let rcu_clone = Arc::clone(&rcu);
            // let value = tx.clone();
            let handle = thread::spawn(move || {
                rcu_clone.write(|data| {
                    *data = *data + 1;
                })
            });
            handles.push(handle);
        }

        // rx.recv().unwrap();
        for handle in handles {
            handle.join().unwrap();
        }

        let reader = rcu.read();

        assert_eq!(*reader, 4);
    }

    #[test]
    fn test_reader_count_cleanup() {
        let rcu = RCUData::new(33);

        {
            let _reader = rcu.read();
            assert_eq!(rcu.reader_count.load(Ordering::SeqCst), 1);
        }
        assert_eq!(rcu.reader_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_reader_cleanup_when_panic() {
        let rcu = Arc::new(RCUData::new("kill_me".to_string()));
        let rcu_clone = Arc::clone(&rcu);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _reader = rcu_clone.read();
            assert_eq!(rcu_clone.reader_count.load(Ordering::SeqCst), 1);
            panic!("in the streets of london");
        }));

        assert!(result.is_err());
        assert_eq!(rcu.reader_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_concurrent_read_write() {
        let rcu = Arc::new(RCUData::new(33));
        let mut handles = vec![];

        for _ in 0..3 {
            let rcu_clone = Arc::clone(&rcu);
            let handle = thread::spawn(move || {
                for _ in 0..5 {
                    let reader = rcu_clone.read();
                    let _data = *reader;
                    thread::sleep(std::time::Duration::from_millis(500));
                }
            });
            handles.push(handle);
        }

        for _ in 0..2 {
            let rce_clone = Arc::clone(&rcu);
            let handle = thread::spawn(move || {
               rce_clone.write(|data| {
                   *data = *data + 2;

               })
            });
            handles.push(handle)
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let reader = rcu.read();
        assert!(*reader >= 33);
    }
}

// price is in cents.
#[derive(Clone, Debug)]
struct MarketData{
    current_price: u64,
    timestamp: u64
}

impl MarketData {
    pub  fn new(price: u64, time: u64) -> Self {
        MarketData{
            current_price: price,
            timestamp: time,
        }
    }
}

// fn main() {
//     let market_data = Arc::new(RCUData::new(MarketData::new(300, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs())));
//     let mut handles = vec![];
//
//
//
//     for trader_id in 0..5 {
//         let market_data_clone = Arc::clone(&market_data);
//         let handle = thread::spawn(move || {
//
//             for trades in 0..50 {
//                 let prices = market_data_clone.read();
//                 println!("Current Price {}", prices.current_price);
//                 if prices.current_price > 400 {
//                     println!(" Sell at id {}", trades)
//                 }
//                 if prices.current_price < 200 {
//                     println!(" Buy at id {}", trades)
//                 }
//                 thread::sleep(std::time::Duration::from_millis(100));
//             }
//
//         });
//         handles.push(handle);
//     }
//
//     let market_clone = Arc::clone(&market_data);
//     let price_updater = thread::spawn(move || {
//
//         let mut price_updates = vec![500, 300, 200, 100, 350, 330, 250, 110, 450, 259];
//
//         for (idx, price) in price_updates.iter().enumerate() {
//             thread::sleep(std::time::Duration::from_millis(250));
//             eprintln!("[UPDATER] About to write price {}", price);
//             market_clone.write(|data| {
//                 data.current_price = *price;
//                 data.timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
//             });
//             // thread::sleep(std::time::Duration::from_millis(150));
//             println!("Market Data {}, Price {}", idx, price);
//         }
//     });
//
//     let final_price = market_data.read();
//
//     println!("The final Price is {:?}", *final_price.data);
//
//
//     for handle in handles {
//         handle.join().unwrap()
//     }
//     price_updater.join().unwrap();
// }

use std::io::{self, Write};

fn main() {
    let market_data = Arc::new(RCUData::new(MarketData::new(
        300,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )));

    for trader_id in 0..2 {
        let market_data_clone = Arc::clone(&market_data);
        thread::spawn(move || {
            for trades in 0..50 {
                {
                    let prices = market_data_clone.read();
                    println!("Trader {} - Price: {}", trader_id, prices.current_price);
                    io::stdout().flush().unwrap();

                    if prices.current_price > 400 {
                        println!("  → Trader {} SELL!", trader_id);
                    }
                    if prices.current_price < 200 {
                        println!("  → Trader {} BUY!", trader_id);
                    }
                }

                thread::sleep(std::time::Duration::from_millis(100));
            }
        });
    }

    let market_clone = Arc::clone(&market_data);
    let price_updater = thread::spawn(move || {
        let prices = vec![500, 200, 100, 350, 450];

        for (idx, price) in prices.iter().enumerate() {
            thread::sleep(std::time::Duration::from_millis(250));

            eprintln!("[WRITE START] Price {} - reader_count: {}",
                      price, market_clone.reader_count.load(Ordering::SeqCst));
            io::stderr().flush().unwrap();

            market_clone.write(|data| {
                data.current_price = *price;
            });

            println!("[MARKET UPDATE {}] Price → {}", idx, price);
            io::stdout().flush().unwrap();
        }
    });

    price_updater.join().unwrap();
    thread::sleep(std::time::Duration::from_secs(6));
}