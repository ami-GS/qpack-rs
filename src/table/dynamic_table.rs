use std::{collections::VecDeque, error, sync::{Arc, Condvar, Mutex}};

use crate::{DecompressionFailed, EncoderStreamError, Header, types::DynamicHeader};

#[derive(Clone)]
pub struct Entry {
    header: Box<DynamicHeader>,
    size: usize,
    outstanding_count: usize,
}
impl Entry {
    pub fn new(header: Box<DynamicHeader>) -> Self {
        let size = header.size();
        Self {
            header,
            size,
            outstanding_count: 0,
        }
    }
    pub fn duplicate(entry: Entry) -> Self {
        Self {
            header: entry.header.clone(),
            size: entry.size,
            outstanding_count: 0,
        }
    }
    pub fn refer_name(entry: Entry, value: String) -> Self {
        let header = Box::new(DynamicHeader(entry.header.0, value));
        let size = header.size();
        Self {
            header,
            size,
            outstanding_count: 0,
        }
    }
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
        Entry::new(Box::new(DynamicHeader::from_str("NOT_FOUND", "NOT_FOUND")))
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
    pub fn ack_section(&mut self, section: usize, ids: Vec<usize>) {
        ids.iter().for_each(|id| {
            let _ = self.deref_entry_at(*id);
        });
        self.known_received_count = section;
    }
    pub fn is_insertable(&self, headers: &Vec<Header>) -> bool {
        let mut size = 0;
        for header in headers {
            size += header.size();
        }
        let upto = if self.capacity < size {0} else {self.capacity - size};
        self.is_evictable_upto(upto)
    }
    fn is_evictable_upto(&self, upto: usize) -> bool {
        let mut current_size = self.current_size;
        let mut idx = 0;
        while idx < self.list.len() && upto < current_size {
            let entry = &self.list[idx];
            if entry.outstanding_count > 0 || self.known_received_count < idx {
                return false;
            }
            current_size -= entry.size;
            idx += 1;
        }
        true
    }
    fn evict_upto(&mut self, upto: usize) -> Result<(), Box<dyn error::Error>> {
        let mut current_size = self.current_size;
        let mut idx = 0;
        while upto < current_size {
            if self.known_received_count < idx {
                // trying to evict non-evictable entry
                return Err(EncoderStreamError.into())
            }
            let entry = &self.list[idx];
            current_size -= entry.size;
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
            println!("\tAbs:{}, Refs:{}, ({}={})", idx, entry.outstanding_count, entry.header.0, entry.header.1);
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
            if (&*entry.header.0).eq(&target.0) {
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
    pub fn ref_entry_at(&mut self, idx: usize) -> Result<(), Box<dyn error::Error>> {
        match self.list.get_mut(idx) {
            Some(entry) => entry.outstanding_count += 1,
            None => return Err(DecompressionFailed.into())
        }
        Ok(())
    }
    pub fn deref_entry_at(&mut self, idx: usize) -> Result<(), Box<dyn error::Error>> {
        match self.list.get_mut(idx) {
            Some(entry) => entry.outstanding_count -= 1,
            None => return Err(DecompressionFailed.into())
        }
        Ok(())
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
        self.insert_table_entry(Entry::new(Box::new(header.into())))
    }
    pub fn get_entry(&self, abs_idx: usize) -> Result<Entry, Box<dyn error::Error>> {
        match self.list.get(abs_idx) {
            Some(entry) => Ok((*entry).clone()),
            None => Err(DecompressionFailed.into())
        }
    }
    pub fn get(&self, abs_idx: usize) -> Result<Header, Box<dyn error::Error>> {
        match self.list.get(abs_idx) {
            Some(entry) => Ok(Header::from((*entry.header).clone())),
            None => Err(DecompressionFailed.into())
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

#[cfg(test)]
mod test {
    use std::sync::{Arc, Condvar, Mutex};
    const MAX_TABLE_SIZE: usize = 1024;
    use crate::{DecompressionFailed, EncoderStreamError, Header, table::dynamic_table::DynamicHeader};

    use super::{DynamicTable, Entry};
    fn gen_table() -> DynamicTable {
        let cv = Arc::new((Mutex::new(0), Condvar::new()));
        DynamicTable::new(MAX_TABLE_SIZE, cv)
    }

    #[test]
    fn set_capacity() {
        let cap = 512;
        let mut table = gen_table();
        let out = table.set_capacity(cap);
        assert_eq!(out.unwrap(), ());
        assert_eq!(table.capacity, cap);
    }

    #[test]
    fn set_capacity_err() {
        let cap = MAX_TABLE_SIZE + 1;
        let mut table = gen_table();
        let out = table.set_capacity(cap).unwrap_err();
        assert!(out.downcast_ref::<EncoderStreamError>().is_some());
    }

    fn verify_insert(table: &DynamicTable, expected_size: usize, expected_insert_count: usize, expected_list_len: usize) {
        assert_eq!(table.current_size, expected_size);
        let (mux, _) = &*table.cv;
        let insert_count = mux.lock().unwrap();
        assert_eq!(*insert_count, expected_insert_count);
        assert_eq!(table.list.len(), expected_list_len);
    }

    #[test]
    fn insert_header() {
        let cap = 512;
        let mut table = gen_table();
        let _ = table.set_capacity(cap);
        let header = Header::from_str(":path", "/index.html");
        let out = table.insert_header(header.clone());
        assert_eq!(out.unwrap(), ());
        verify_insert(&table, header.size(), 1, 1);
    }

    #[test]
    fn insert_header_err_bigger_than_cap() {
        let cap = 10;
        let mut table = gen_table();
        let _ = table.set_capacity(cap);
        let header = Header::from_str(":path", "/index.html");
        let out = table.insert_header(header.clone()).unwrap_err();
        assert!(out.downcast_ref::<EncoderStreamError>().is_some());
        verify_insert(&table, 0, 0, 0);
    }
    #[test]
    fn insert_table_entry() {
        let cap = 512;
        let mut table = gen_table();
        let _ = table.set_capacity(cap);
        let header = Box::new(DynamicHeader::from_str(":path", "/index.html"));
        let size = header.size();
        let out = table.insert_table_entry(Entry::new(header));
        assert_eq!(out.unwrap(), ());
        verify_insert(&table, size, 1, 1);
    }
    #[test]
    fn insert_table_entry_bigger_than_cap() {
        let cap = 10;
        let mut table = gen_table();
        let _ = table.set_capacity(cap);
        let header = Box::new(DynamicHeader::from_str(":path", "/index.html"));
        let out = table.insert_table_entry(Entry::new(header)).unwrap_err();
        assert!(out.downcast_ref::<EncoderStreamError>().is_some());
        verify_insert(&table, 0, 0, 0);
    }
    #[test]
    fn get() {
        let cap = 512;
        let mut table = gen_table();
        let _ = table.set_capacity(cap);
        let headers = vec![
            Header::from_str(":path", "/index.html"),
            Header::from_str("TARGET_KEY", "TARGET_VALUE"),
            Header::from_str(":method", "GET"),
        ];
        for header in &headers {
            let _ = table.insert_header(header.clone());
        }
        let header = table.get(1);
        assert_eq!(header.unwrap(), headers[1]);
    }
    #[test]
    fn get_not_found() {
        let table = gen_table();
        let out = table.get(128).unwrap_err();
        assert!(out.downcast_ref::<DecompressionFailed>().is_some());
    }
}