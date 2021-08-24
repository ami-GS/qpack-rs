mod dynamic_table;

use std::error;
use std::sync::{Arc, Condvar, Mutex, RwLock};

use crate::types::StrHeader;
use crate::{DecompressionFailed, Header};

use self::dynamic_table::{DynamicTable, Entry};

pub struct Table {
    pub dynamic_table: Arc<RwLock<DynamicTable>>,
}

impl Table {
    pub fn new(max_capacity: usize, cv: Arc<(Mutex<usize>, Condvar)>) -> Self {
        Self {
            dynamic_table: Arc::new(RwLock::new(DynamicTable::new(max_capacity, cv))),
        }
    }
    // TODO: return (both_matched, on_static_table, idx)
    //       try to remove on_static_table as my HPACK did not use
    pub fn find_index(&self, target: &Header) -> (bool, bool, usize) {
        let not_found_val = usize::MAX;

        let mut static_candidate_idx: usize = not_found_val;
        for (idx, (name, val)) in STATIC_TABLE.iter().enumerate() {
            if target.0.eq(*name) {
                if target.1.eq(*val) {
                    // match both
                    return (true, true, idx);
                }
                if static_candidate_idx == not_found_val {
                    static_candidate_idx = idx;
                } else if STATIC_TABLE[static_candidate_idx].0.ne(*name) {
                    // match name
                    return (false, true, static_candidate_idx);
                }
            }
        }

        let ret = self.dynamic_table.read().unwrap().find_index(target);
        if ret.1 == not_found_val && static_candidate_idx != not_found_val {
            return (false, true, static_candidate_idx);
        }
        (ret.0, false, ret.1) // (false, false, usize::MAX) means not found
    }
    pub fn get_header_from_static(&self, idx: usize) -> Result<Header, Box<dyn error::Error>> {
        if STATIC_TABLE_SIZE <= idx {
            return Err(DecompressionFailed.into());
        }
        Ok(Header::from_str_header(STATIC_TABLE[idx]))
    }
    fn calc_abs_index(&self, base: usize, idx: usize, post_base: bool) -> usize {
        if post_base {
            base + idx
        } else {
            base - idx - 1
        }
    }
    pub fn get_header_from_dynamic(&self, base: usize, idx: usize, post_base: bool) -> Result<Header, Box<dyn error::Error>> {
        self.dynamic_table.read().unwrap().get(self.calc_abs_index(base, idx, post_base))
    }
    pub fn get_entry_from_dynamic(&self, base: usize, idx: usize, post_base: bool) -> Result<Entry, Box<dyn error::Error>> {
        self.dynamic_table.read().unwrap().get_entry(self.calc_abs_index(base, idx, post_base))
    }
    pub fn set_dynamic_table_capacity(&self, capacity: usize)
        -> Result<Box<dyn FnOnce() -> Result<(), Box<dyn error::Error>>>,
                    Box<dyn error::Error>> {
        let dynamic_table = Arc::clone(&self.dynamic_table);
        Ok(Box::new(move || -> Result<(), Box<dyn error::Error>> {
            dynamic_table.write().unwrap().set_capacity(capacity)
        }))
    }
    pub fn insert_with_name_reference(&self, idx: usize, value: String, on_static: bool)
        -> Result<Box<dyn FnOnce() -> Result<(), Box<dyn error::Error>>>,
                    Box<dyn error::Error>> {
        let dynamic_table = Arc::clone(&self.dynamic_table);
        if on_static {
            let name = self.get_header_from_static(idx)?.0.to_string();
            return Ok(Box::new(move || -> Result<(), Box<dyn error::Error>> {
                dynamic_table.write().unwrap().insert_header(Header::from_string(name, value))
            }));
        }
        let entry = self.get_entry_from_dynamic(self.get_insert_count(), idx, false)?;
        return Ok(Box::new(move || -> Result<(), Box<dyn error::Error>> {
            dynamic_table.write().unwrap().insert_table_entry(Entry::refer_name(entry, value))
        }));
    }
    pub fn insert_with_literal_name(&self, name: String, value: String)
        -> Result<Box<dyn FnOnce() -> Result<(), Box<dyn error::Error>>>,
                    Box<dyn error::Error>> {
        let dynamic_table = Arc::clone(&self.dynamic_table);
        Ok(Box::new(move || -> Result<(), Box<dyn error::Error>> {
            dynamic_table.write().unwrap().insert_header(Header::from_string(name, value))
        }))
    }
    pub fn duplicate(&self, idx: usize)
        -> Result<Box<dyn FnOnce() -> Result<(), Box<dyn error::Error>>>,
                    Box<dyn error::Error>> {
        let entry = self.get_entry_from_dynamic(self.get_insert_count(), idx, false)?;
        let dynamic_table = Arc::clone(&self.dynamic_table);
        Ok(Box::new(move || -> Result<(), Box<dyn error::Error>> {
            dynamic_table.write().unwrap().insert_table_entry(Entry::duplicate(entry))
        }))
    }
    pub fn get_max_entries(&self) -> u32 {
        (self.dynamic_table.read().unwrap().max_capacity as f64 / 32 as f64).floor() as u32
    }
    pub fn get_insert_count(&self) -> usize {
        self.dynamic_table.read().unwrap().get_insert_count()
    }
    pub fn get_dynamic_table_entry_len(&self) -> usize {
        self.dynamic_table.read().unwrap().get_entry_len()
    }
    pub fn dump_dynamic_table(&self) {
        self.dynamic_table.read().unwrap().dump_entries();
    }
}

const STATIC_TABLE_SIZE: usize = 99;
const STATIC_TABLE: [StrHeader; STATIC_TABLE_SIZE] = [
    (":authority", ""),
    (":path", "/"),
    ("age", "0"),
    ("content-disposition", ""),
    ("content-length", "0"),
    ("cookie", ""),
    ("date", ""),
    ("etag", ""),
    ("if-modified-since", ""),
    ("if-none-match", ""),
    ("last-modified", ""),
    ("link", ""),
    ("location", ""),
    ("referer", ""),
    ("set-cookie", ""),
    (":method", "CONNECT"),
    (":method", "DELETE"),
    (":method", "GET"),
    (":method", "HEAD"),
    (":method", "OPTIONS"),
    (":method", "POST"),
    (":method", "PUT"),
    (":scheme", "http"),
    (":scheme", "https"),
    (":status", "103"),
    (":status", "200"),
    (":status", "304"),
    (":status", "404"),
    (":status", "503"),
    ("accept", "*/*"),
    ("accept", "application/dns-message"),
    ("accept-encoding", "gzip, deflate, br"),
    ("accept-ranges", "bytes"),
    ("access-control-allow-headers", "cache-control"),
    ("access-control-allow-headers", "content-type"),
    ("access-control-allow-origin", "*"),
    ("cache-control", "max-age=0"),
    ("cache-control", "max-age=2592000"),
    ("cache-control", "max-age=604800"),
    ("cache-control", "no-cache"),
    ("cache-control", "no-store"),
    ("cache-control", "public, max-age=31536000"),
    ("content-encoding", "br"),
    ("content-encoding", "gzip"),
    ("content-type", "application/dns-message"),
    ("content-type", "application/javascript"),
    ("content-type", "application/json"),
    ("content-type", "application/x-www-form-urlencoded"),
    ("content-type", "image/gif"),
    ("content-type", "image/jpeg"),
    ("content-type", "image/png"),
    ("content-type", "text/css"),
    ("content-type", "text/html; charset=utf-8"),
    ("content-type", "text/plain"),
    ("content-type", "text/plain;charset=utf-8"),
    ("range", "bytes=0-"),
    ("strict-transport-security", "max-age=31536000"),
    (
        "strict-transport-security",
        "max-age=31536000; includesubdomains",
    ),
    (
        "strict-transport-security",
        "max-age=31536000; includesubdomains; preload",
    ),
    ("vary", "accept-encoding"),
    ("vary", "origin"),
    ("x-content-type-options", "nosniff"),
    ("x-xss-protection", "1; mode=block"),
    (":status", "100"),
    (":status", "204"),
    (":status", "206"),
    (":status", "302"),
    (":status", "400"),
    (":status", "403"),
    (":status", "421"),
    (":status", "425"),
    (":status", "500"),
    ("accept-language", ""),
    ("access-control-allow-credentials", "FALSE"),
    ("access-control-allow-credentials", "TRUE"),
    ("access-control-allow-headers", "*"),
    ("access-control-allow-methods", "get"),
    ("access-control-allow-methods", "get, post, options"),
    ("access-control-allow-methods", "options"),
    ("access-control-expose-headers", "content-length"),
    ("access-control-request-headers", "content-type"),
    ("access-control-request-method", "get"),
    ("access-control-request-method", "post"),
    ("alt-svc", "clear"),
    ("authorization", ""),
    (
        "content-security-policy",
        "script-src 'none'; object-src 'none'; base-uri 'none'",
    ),
    ("early-data", "1"),
    ("expect-ct", ""),
    ("forwarded", ""),
    ("if-range", ""),
    ("origin", ""),
    ("purpose", "prefetch"),
    ("server", ""),
    ("timing-allow-origin", "*"),
    ("upgrade-insecure-requests", "1"),
    ("user-agent", ""),
    ("x-forwarded-for", ""),
    ("x-frame-options", "deny"),
    ("x-frame-options", "sameorigin"),
];