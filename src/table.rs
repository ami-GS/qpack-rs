use std::error;
use std::sync::{Arc, Condvar, Mutex};

use crate::dynamic_table::DynamicTable;
use crate::{DecompressionFailed, Header, StrHeader};

pub struct Table {
    pub dynamic_table: DynamicTable,
    static_table: &'static [StrHeader<'static>; 99],
}

impl Table {
    pub fn new(max_capacity: usize, cv: Arc<(Mutex<usize>, Condvar)>) -> Self {
        Self {
            dynamic_table: DynamicTable::new(max_capacity, cv),
            static_table: &STATIC_TABLE,
        }
    }
    pub fn ack_section(&mut self, section: usize) {
        self.dynamic_table.ack_section(section);
    }

    // TODO: return (both_matched, on_static_table, idx)
    //       try to remove on_static_table as my HPACK did not use
    pub fn find_index(&self, target: &Header) -> (bool, bool, usize) {
        let not_found_val = (1 << 32) - 1; // TODO: need to set invalid value

        let mut static_candidate_idx: usize = not_found_val;
        for (idx, (name, val)) in self.static_table.iter().enumerate() {
            if target.0.eq(*name) {
                if target.1.eq(*val) {
                    // match both
                    return (true, true, idx);
                }
                if static_candidate_idx == not_found_val {
                    static_candidate_idx = idx;
                } else if self.static_table[static_candidate_idx].0.ne(*name) {
                    // match name
                    return (false, true, static_candidate_idx);
                }
            }
        }

        let ret = self.dynamic_table.find_index(target);
        if ret.1 == not_found_val && static_candidate_idx != not_found_val {
            return (false, true, static_candidate_idx);
        }
        (ret.0, false, ret.1) // (false, false, (1 << 32 - 1)) means not found
    }

    pub fn get_from_static(&self, idx: usize) -> Result<Header, Box<dyn error::Error>> {
        if STATIC_TABLE_SIZE <= idx {
            return Err(DecompressionFailed.into());
        }
        let header = self.static_table[idx];
        Ok(Header::from_str_header(header))
    }
    pub fn get_from_dynamic(&self, idx: usize, post_base: bool) -> Result<Header, Box<dyn error::Error>> {
        self.dynamic_table.get(idx, post_base, true)
    }
    pub fn get_from_dynamic_with_base(&self, base: usize, idx: usize, post_base: bool) -> Result<Header, Box<dyn error::Error>> {
        let idx = if post_base {
            base + idx
        } else {
            base - idx - 1
        };
        self.dynamic_table.get(idx, post_base, false)
    }
    pub fn set_dynamic_table_capacity(&mut self, cap: usize) -> Result<(), Box<dyn error::Error>> {
        self.dynamic_table.set_capacity(cap)
    }
    pub fn insert_with_name_reference(&mut self, name_idx: usize, value: String, on_static_table: bool) -> Result<(), Box<dyn error::Error>> {
        let name = if on_static_table {
            self.static_table[name_idx].0.to_string()
        } else {
            self.get_from_dynamic(name_idx, false)?.0
        };
        self.dynamic_table.insert(Header::from_string(name, value))
    }
    pub fn insert_with_literal_name(&mut self, name: String, value: String) -> Result<(), Box<dyn error::Error>> {
        self.dynamic_table.insert(Header::from_string(name, value))
    }
    pub fn duplicate(&mut self, index: usize) -> Result<(), Box<dyn error::Error>> {
        // TODO: really?
        //       abs = insert count - index - 1;
        //       base is treated as insert count
        //       the +1 comes from that the "insert count" might include currently comming insert
        let header = self.get_from_dynamic(index, false)?;
        self.insert_with_literal_name(header.0, header.1)
    }
    pub fn insert(&mut self, header: Header) -> Result<(), Box<dyn error::Error>> {
        self.dynamic_table.insert(header)
    }
    pub fn get_max_entries(&self) -> u32 {
        (self.dynamic_table.max_capacity as f64 / 32 as f64).floor() as u32
    }
    pub fn get_insert_count(&self) -> usize {
        self.dynamic_table.get_insert_count()
    }
    pub fn dump_dynamic_table(&self) {
        self.dynamic_table.dump_entries();
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