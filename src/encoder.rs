use std::error;
use std::ops::Index;

use crate::{table::Table, Header};
use crate::{FieldType, Qnum, Setting};

struct Instruction;
impl Instruction {
    pub const SetDynamicTableCapacity: u8 = 0b00100000;
    pub const InsertWithNameReference: u8 = 0b10000000;
    pub const InsertWithLiteralName: u8 = 0b01000000;
    pub const Duplicate: u8 = 0b00000000;
}

pub struct Encoder {
    // $2.1.1.1
    draining_idx: u32,
    known_received_count: u32,
}

impl Encoder {
    pub fn new() -> Self {
        Self {
            draining_idx: 0,
            known_received_count: 0,
        }
    }
    pub fn encode(&self, table: &mut Table, headers: Vec<Header>, dynamic_table_cap: Option<u32>, use_static: bool) -> Vec<u8> {
        let mut encoded = vec![];
        if let Some(cap) = dynamic_table_cap {
            let _ = self.set_dynamic_table_capacity(&mut encoded, cap);
        }
        self.prefix(&mut encoded, table, 0, false, 0);

        for header in headers {
            let (both_match, idx) = table.find_index(&header);
            if both_match {
                self.indexed(&mut encoded, table, idx as u32, true);
                // self.postBaseIndex(&mut encoded, idx);
            } else {
                //self.literal_literal_name(&mut encoded, &header);
                self.literal_name_reference(&mut encoded, table, idx as u32, header.1, true);
            }
        }

        encoded
    }
    fn set_dynamic_table_capacity(&self, encoded: &mut Vec<u8>, cap: u32) -> Result<(), Box<dyn error::Error>> {
        let len = Qnum::encode(encoded, cap, 5);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= Setting::QPACK_MAX_TABLE_CAPACITY;
        Ok(())
        // TODO: error handling
    }
    fn insert_with_name_reference(&self) {
    }
    fn prefix(&self, encoded: &mut Vec<u8>, table: &Table, required_insert_count: u32, s_flag: bool, base: u32) {
        Qnum::encode(encoded, required_insert_count, 8);

        // S=1: req > base if insert/reference dynamic table
        // S=0: base > req if do not
        // S=0: base = req
        // base can be any if no reference to dynamic table. Delta Base to 0 is the most efficient
        // S=0 and delta base 0 case
        let delta_base = if s_flag {
            required_insert_count - base - 1
        } else {
            base - required_insert_count
        };
        Qnum::encode(encoded, delta_base, 7);
    }

    fn indexed(&self, encoded: &mut Vec<u8>, table: &mut Table, idx: u32, from_static: bool) {
        let len = Qnum::encode(encoded, idx, 6);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= FieldType::Indexed;
        if from_static {
            let wire_len = encoded.len();
            encoded[wire_len - len] |= 0b01000000;
        }
    }
    fn indexed_post_base_index(&self, encoded: &mut Vec<u8>, idx: u32) {
        let len = Qnum::encode(encoded, idx, 4);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= FieldType::IndexedPostBaseIndex;
    }
    fn literal_name_reference(
        &self,
        encoded: &mut Vec<u8>,
        table: &mut Table,
        idx: u32,
        value: &str,
        from_static: bool,
    ) {
        // TODO: "N" bit?
        let len = Qnum::encode(encoded, idx, 4);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= FieldType::LiteralNameReference;
        if from_static {
            let wire_len = encoded.len();
            encoded[wire_len - len] |= 0b00010000;
        }
        // TODO: "H" bit?
        let _ = Qnum::encode(encoded, value.len() as u32, 7);
        encoded.append(&mut value.as_bytes().to_vec());
    }
    fn literal_post_base_name_reference(&self, encoded: &mut Vec<u8>, idx: u32, value: &str) {
        // TODO: "N" bit?
        let len = Qnum::encode(encoded, idx, 3);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= FieldType::LiteralPostBaseNameReference;
        // TODO: "H" bit?
        let _ = Qnum::encode(encoded, value.len() as u32, 7);
        encoded.append(&mut value.as_bytes().to_vec());
    }
    fn literal_literal_name(&self, encoded: &mut Vec<u8>, header: &(&str, &str)) {
        // TODO: "N", "H" bit?
        let len = Qnum::encode(encoded, header.0.len() as u32, 3);
        let wire_len  = encoded.len();
        encoded[wire_len - len] |= FieldType::LiteralLiteralName;
        encoded.append(&mut header.0.as_bytes().to_vec());
        // TODO: "H" bit?
        let _ = Qnum::encode(encoded, header.1.len() as u32, 7);
        encoded.append(&mut header.1.as_bytes().to_vec());
    }
}
