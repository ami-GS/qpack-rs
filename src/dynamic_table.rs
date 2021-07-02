use std::{collections::LinkedList, error};

use crate::{DynamicHeader, EncoderStreamError, Header};

struct Entry {
    header: DynamicHeader,
    // Absolute index
    index: usize,
}
pub struct DynamicTable {
    list: LinkedList<Entry>,
    pub current_size: usize,
    pub insert_count: usize,
    pub capacity: usize,
    // set by SETTINGS_QPACK_MAX_TABLE_CAPACITY in SETTINGS frame
    pub max_capacity: usize,
}

impl<'a> DynamicTable {
    pub fn new(max_capacity: usize) -> Self {
        Self {
            list: LinkedList::new(),
            current_size: 0,
            insert_count: 0,
            capacity: 0,
            max_capacity
        }
    }
    fn evict_upto(&mut self, upto: usize) {
        while upto < self.current_size {
            if let Some(elm) = self.list.pop_back() {
                self.current_size -= elm.header.0.len() + elm.header.1.len() + 32;
            } else {
                // error
            }
        }
    }
    pub fn find_index(&self, target: &Header) -> (bool, usize) {
        let mut candidate_idx = (1 << 32) - 1;
        if self.current_size == 0 {
            return (false, candidate_idx);
        }
        let mut abs_idx = self.insert_count - 1;
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
        let size = header.0.len() + header.1.len() + 32;
        if self.capacity < size {
            return Err(EncoderStreamError.into());
        }
        // copy before eviction to avoid referenced entry to be deleted;
        let dyn_header = (header.0.to_string(), header.1.to_string());
        self.evict_upto(self.capacity - size);
        self.list.push_front(Entry{header: dyn_header, index: self.insert_count});
        self.insert_count += 1;
        self.current_size += size;
        Ok(())
        //Err(EncoderStreamError.into())
    }
    pub fn get(&self, idx: usize) -> Result<Header<'a>, Box<dyn error::Error>> {
        Ok(("TODO", "TODO"))
    }
    pub fn set_capacity(&mut self, cap: usize) -> Result<(), Box<dyn error::Error>> {
        if self.max_capacity < cap {
            return Err(EncoderStreamError.into());
        }
        self.evict_upto(cap);
        self.capacity = cap;
        // error when to set 0. see $3.2.3
        // error when exceed limit as QPACK_ENCODER_STREAM_ERROR?
        // Err(EncoderStreamError.into())
        Ok(())
    }
}
