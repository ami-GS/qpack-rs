use std::{collections::LinkedList, error, sync::{Arc, Condvar, Mutex}};

use crate::{EncoderStreamError, Header};

pub struct Entry {
    pub header: Header,
    // Absolute index
    _index: usize,
}
pub struct DynamicTable {
    pub list: LinkedList<Entry>,
    pub current_size: usize,
    pub capacity: usize,
    // # 2.1.4
    // The Known Received Count is the total number of dynamic table insertions and duplications acknowledged by the decoder
    pub known_received_count: usize,
    // set by SETTINGS_QPACK_MAX_TABLE_CAPACITY in SETTINGS frame
    pub max_capacity: usize,
    cv: Arc<(Mutex<usize>, Condvar)>,
}

impl DynamicTable {
    pub fn new(max_capacity: usize, cv: Arc<(Mutex<usize>, Condvar)>) -> Self {
        Self {
            list: LinkedList::new(),
            current_size: 0,
            capacity: 0,
            known_received_count: 0,
            max_capacity,
            cv
        }
    }
    pub fn get_insert_count(&self) -> usize {
        let (mux, _) = &*self.cv;
        *mux.lock().unwrap()
    }
    fn increment_insert_count(&mut self) {
        let (mux, cv) = &*self.cv;
        let mut insert_count = mux.lock().unwrap();
        *insert_count += 1;
        cv.notify_all();
    }
    pub fn ack_section(&mut self, section: usize) {
        self.known_received_count = section;
    }
    fn evict_upto(&mut self, upto: usize) -> Result<(), Box<dyn error::Error>> {
        let mut current_size = self.current_size;
        let mut idx = 0;
        for elm in self.list.iter() {
            if self.known_received_count < idx {
                // trying to evict non-evictable entry
                return Err(EncoderStreamError.into());
            }
            if current_size <= upto {
                break;
            }
            current_size -= elm.header.0.len() + elm.header.1.len() + 32;
            idx += 1;
        }

        while idx > 0 {
            self.list.pop_back();
            idx -= 1;
        }
        self.current_size = current_size;

        Ok(())
    }
    pub fn dump_entries(&self) {
        // TODO: selective output target to do test table contents
        let insert_count = self.get_insert_count();
        println!("Insert Count:{}, Current Size: {}", insert_count, self.current_size);
        let mut idx = insert_count-1;
        for entry in self.list.iter() {
            if idx + 1 == self.known_received_count {
                println!("v-------- acked sections --------v");
            }
            println!("\tAbs:{} ({}={})", idx, entry.header.0, entry.header.1);
            if idx != 0 {
                idx -= 1;
            }
        }
    }
    pub fn find_index(&self, target: &Header) -> (bool, usize) {
        let insert_count = self.get_insert_count();
        let mut candidate_idx = (1 << 32) - 1;
        if self.current_size == 0 {
            return (false, candidate_idx);
        }
        let mut abs_idx = insert_count - 1;
        for entry in self.list.iter() {
            if entry.header.0.eq(&target.0) {
                if entry.header.1.eq(&target.1) {
                    return (true, abs_idx);
                }
                candidate_idx = abs_idx;
            }
            if abs_idx != 0 {
                abs_idx -= 1;
            }
        }
        (false, candidate_idx)
    }
    pub fn insert(&mut self, header: Header) -> Result<(), Box<dyn error::Error>> {
        // TODO: would be able to remove this get
        let insert_count = self.get_insert_count();
        let size = header.0.len() + header.1.len() + 32;
        if self.capacity < size {
            return Err(EncoderStreamError.into());
        }
        // copy before eviction to avoid referenced entry to be deleted;
        // let dyn_header = (header.0.to_string(), header.1.to_string());
        self.evict_upto(self.capacity - size)?;
        self.list.push_front(Entry{header: header, _index: insert_count});

        self.increment_insert_count();

        self.current_size += size;
        Ok(())
    }
    pub fn get(&self, relative_idx: usize, post_base: bool, calc_idx: bool) -> Result<Header, Box<dyn error::Error>> {
        let insert_count = self.get_insert_count();
        let abs_idx = if calc_idx {
            if post_base {
                insert_count + relative_idx
            } else {
                insert_count - relative_idx - 1
            }
        } else {
            relative_idx
        };
        let mut i = insert_count;
        for entry in self.list.iter() {
            if abs_idx == i-1 {
                return Ok(Header::from(&entry.header.0, &entry.header.1));
            }
            i -= 1;
        }
        // TODO: error
        Ok(Header::from("NOT_FOUND", "NOT_FOUND"))
    }
    pub fn set_capacity(&mut self, cap: usize) -> Result<(), Box<dyn error::Error>> {
        if self.max_capacity < cap {
            return Err(EncoderStreamError.into());
        }
        self.evict_upto(cap)?;
        self.capacity = cap;
        // error when to set 0. see $3.2.3
        // error when exceed limit as QPACK_ENCODER_STREAM_ERROR?
        // Err(EncoderStreamError.into())
        Ok(())
    }
}
