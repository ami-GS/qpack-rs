use std::{collections::VecDeque, error, sync::{Arc, Condvar, Mutex}};

use crate::{EncoderStreamError, Header};

#[derive(Clone)]
pub struct Entry {
    header: Box<Header>,
    size: usize,
}
pub struct DynamicTable {
    pub list: VecDeque<Entry>,
    pub current_size: usize,
    pub capacity: usize,
    // # 2.1.4
    // The Known Received Count is the total number of dynamic table insertions and duplications acknowledged by the decoder
    pub known_received_count: usize,
    // set by SETTINGS_QPACK_MAX_TABLE_CAPACITY in SETTINGS frame
    pub max_capacity: usize,
    cv: Arc<(Mutex<usize>, Condvar)>,
}

lazy_static! {
    pub static ref ERROR_ENTRY: Entry = {
        Entry{header: Header::from("NOT_FOUND", "NOT_FOUND").into(), size: 0}
    };
}

impl DynamicTable {
    pub fn new(max_capacity: usize, cv: Arc<(Mutex<usize>, Condvar)>) -> Self {
        Self {
            list: VecDeque::<Entry>::new(),
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
    pub fn get_entry_len(&self) -> usize {
        self.list.len()
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
    fn evict_upto(&mut self, upto: usize) -> Result<(), Box<dyn error::Error>>{
        let mut current_size = self.current_size;
        let mut idx = 0;
        while upto < current_size {
            if self.known_received_count < idx {
                // trying to evict non-evictable entry
                return Err(EncoderStreamError.into())
            }
            let header = &self.list[idx];
            current_size -= header.size;
            idx += 1;
        }
        while idx > 0 {
            self.list.pop_front();
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
        for entry in self.list.iter().rev() {
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
        // TODO: really bad design due to linked list
        let mut candidate_idx = usize::MAX;
        if self.current_size == 0 {
            return (false, candidate_idx);
        }
        // relative from base opposit end
        let mut abs_idx = self.list.len();
        for entry in self.list.iter().rev() {
            if entry.header.0.eq(&target.0) {
                if entry.header.1.eq(&target.1) {
                    return (true, abs_idx-1);
                }
                if candidate_idx == usize::MAX {
                    candidate_idx = abs_idx-1;
                }
            }
            abs_idx -= 1;
        }
        (false, candidate_idx)
    }
    pub fn insert_table_entry(&mut self, entry: Entry) -> Result<(), Box<dyn error::Error>> {
        let size = entry.size;
        if self.capacity < size {
            return Err(EncoderStreamError.into());
        }
        self.evict_upto(self.capacity - size)?;
        self.list.push_back(entry);

        self.increment_insert_count();

        self.current_size += size;
        Ok(())
    }
    // TODO: insert to diverse for each type (ref, copy etc.)
    pub fn insert_header(&mut self, header: Header) -> Result<(), Box<dyn error::Error>> {
        let size = &header.0.len() + &header.1.len() + 32;
        self.insert_table_entry(Entry{header: Box::new(header), size})
    }
    pub fn get_entry(&self, abs_idx: usize) -> Result<Entry, Box<dyn error::Error>> {
        match self.list.get(abs_idx) {
            Some(entry) => Ok((*entry).clone()),
            None => Ok(ERROR_ENTRY.clone())
        }
    }
    pub fn get(&self, abs_idx: usize) -> Result<Header, Box<dyn error::Error>> {
        match self.list.get(abs_idx) {
            Some(entry) => Ok((*entry.header).clone()),
            None => Ok(*ERROR_ENTRY.header.clone())
        }
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
