use std::collections::HashMap;
use std::error;

use crate::{table::Table, Header};
use crate::{FieldType, Qnum};

pub struct Instruction;
impl Instruction {
    pub const SetDynamicTableCapacity: u8 = 0b00100000;
    pub const InsertWithNameReference: u8 = 0b10000000;
    pub const InsertWithLiteralName: u8 = 0b01000000;
    pub const Duplicate: u8 = 0b00000000;
}

pub struct Encoder {
    // $2.1.1.1
    draining_idx: u32,
    pub known_sending_count: usize,
    pub known_received_count: usize,
    pending_sections: HashMap<u16, usize>, // experimental
}

impl Encoder {
    pub fn new() -> Self {
        Self {
            draining_idx: 0,
            known_sending_count: 0,
            known_received_count: 0, // can be covered by acked_section
            pending_sections: HashMap::new(),
        }
    }
    pub fn ack_section(&mut self, stream_id: u16) -> usize {
        // TOOD: remove unwrap
        let section = self.pending_sections.get(&stream_id).unwrap().clone();
        self.pending_sections.remove(&stream_id);
        section
    }
    pub fn encode_headers(&mut self, encoded: &mut Vec<u8>, table: &mut Table, relative_indexing: bool, headers: Vec<Header>, stream_id: u16) -> Result<(), Box<dyn error::Error>> {
        // TODO: decide whether to use s_flag (relative indexing)
        let required_insert_count = table.get_insert_count();
        // TODO: suspicious
        let base = if relative_indexing {
            (*table.dynamic_read).borrow().acked_section as u32
        } else {
            (*table.dynamic_read).borrow().insert_count as u32
        };
        self.prefix(encoded, table, required_insert_count as u32, relative_indexing, base);

        for header in headers {
            let (both_match, on_static, idx) = table.find_index(&header);
            if both_match {
                if relative_indexing {
                    self.indexed_post_base_index(encoded, idx as u32);
                } else {
                    let abs_idx = if on_static { idx } else { base as usize - idx - 1 };
                    self.indexed(encoded, abs_idx as u32, on_static);
                }
            } else if idx != (1 << 32) - 1 {
                if relative_indexing {
                    self.literal_post_base_name_reference(encoded, idx as u32, &header.1);
                } {
                    self.literal_name_reference(encoded, idx as u32, &header.1, on_static);
                }
            } else { // not found
                self.literal_literal_name(encoded, &header);
            }
        }
        self.pending_sections.insert(stream_id, required_insert_count);
        Ok(())
    }

    // Encode Encoder instructions
    pub fn set_dynamic_table_capacity(&self, encoded: &mut Vec<u8>, cap: u32, table: &mut Table) -> Result<(), Box<dyn error::Error>> {
        let len = Qnum::encode(encoded, cap, 5);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= Instruction::SetDynamicTableCapacity;

        table.set_dynamic_table_capacity(cap as usize)
    }
    pub fn insert_with_name_reference(&self, encoded: &mut Vec<u8>, on_static: bool, name_idx: usize, value: String, table: &mut Table) -> Result<(), Box<dyn error::Error>> {
        let len = Qnum::encode(encoded, name_idx as u32, 6);
        let wire_len = encoded.len();
        if on_static { // "T" bit
            encoded[wire_len - len] |= 0b01000000;
        }
        encoded[wire_len - len] |= Instruction::InsertWithNameReference;
        // TODO: "H" bit
        let _ = Qnum::encode(encoded, value.len() as u32, 7);
        encoded.append(&mut value.as_bytes().to_vec());

        table.insert_with_name_reference(name_idx, value.to_string(), on_static)
    }
    pub fn insert_with_literal_name(&self, encoded: &mut Vec<u8>, name: String, value: String, table: &mut Table) -> Result<(), Box<dyn error::Error>> {
        let len = Qnum::encode(encoded, name.len() as u32, 5);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= Instruction::InsertWithLiteralName;
        encoded.append(&mut name.as_bytes().to_vec());
        let _ = Qnum::encode(encoded, value.len() as u32, 7);
        encoded.append(&mut value.as_bytes().to_vec());

        table.insert_with_literal_name(name, value)
    }
    pub fn duplicate(&self, encoded: &mut Vec<u8>, abs_idx: usize, table: &mut Table) -> Result<(), Box<dyn error::Error>> {
        let idx = abs_idx;
        let len  = Qnum::encode(encoded, idx as u32, 5);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= Instruction::Duplicate;

        table.duplicate(idx)
    }

    // Decode Decoder instructions
    pub fn section_ackowledgment(&self, wire: &Vec<u8>, idx: usize) -> Result<(usize, u16), Box<dyn error::Error>> {
        let (len, stream_id) = Qnum::decode(wire, idx, 7);
        Ok((len, stream_id as u16))
    }
    pub fn stream_cancellation(&self, wire: &Vec<u8>, idx: usize) -> Result<(usize, u16), Box<dyn error::Error>> {
        let (len, stream_id) = Qnum::decode(wire, idx, 6);
        Ok((len, stream_id as u16))
    }
    pub fn insert_count_increment(&self, wire: &Vec<u8>, idx: usize) -> Result<(usize, usize), Box<dyn error::Error>> {
        let (len, increment) = Qnum::decode(wire, idx, 6);
        Ok((len, increment as usize))
    }

    fn prefix(&self, encoded: &mut Vec<u8>, table: &Table, required_insert_count: u32, s_flag: bool, base: u32) {
        let encoded_insert_count = if required_insert_count == 0 {
            required_insert_count
        } else {
            required_insert_count % (2 * table.get_max_entries()) + 1
        };
        Qnum::encode(encoded, encoded_insert_count, 8);

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
        let len = Qnum::encode(encoded, delta_base, 7);
        if s_flag {
            let wire_len = encoded.len();
            encoded[wire_len - len] |= 0b10000000;
        }
    }

    fn indexed(&self, encoded: &mut Vec<u8>, idx: u32, from_static: bool) {
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
    fn literal_literal_name(&self, encoded: &mut Vec<u8>, header: &Header) {
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
