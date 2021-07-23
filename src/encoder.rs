use std::collections::HashMap;
use std::error;

use crate::{table::Table, Header};
use crate::{FieldType, Qnum};

pub struct Instruction;
impl Instruction {
    pub const SET_DYNAMIC_TABLE_CAPACITY: u8 = 0b00100000;
    pub const INSERT_WITH_NAME_REFERENCE: u8 = 0b10000000;
    pub const INSERT_WITH_LITERAL_NAME: u8 = 0b01000000;
    pub const DUPLICATE: u8 = 0b00000000;
}

pub struct Encoder {
    // $2.1.1.1
    _draining_idx: u32,
    pub known_sending_count: usize,
    pub pending_sections: HashMap<u16, usize>, // experimental
}

impl Encoder {
    pub fn new() -> Self {
        Self {
            _draining_idx: 0,
            known_sending_count: 0,
            pending_sections: HashMap::new(),
        }
    }
    fn ack_section(&mut self, stream_id: u16) -> usize {
        // TOOD: remove unwrap
        let section = self.pending_sections.get(&stream_id).unwrap().clone();
        self.pending_sections.remove(&stream_id);
        section
    }
    fn cancel_section(&mut self, stream_id: u16) {
        self.pending_sections.remove(&stream_id);
    }
    // Encode Encoder instructions
    pub fn set_dynamic_table_capacity(&self, encoded: &mut Vec<u8>, cap: usize) -> Result<(), Box<dyn error::Error>> {
        let len = Qnum::encode(encoded, cap as u32, 5);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= Instruction::SET_DYNAMIC_TABLE_CAPACITY;
        Ok(())
    }
    pub fn insert_with_name_reference(&self, encoded: &mut Vec<u8>, on_static: bool, name_idx: usize, value: String) -> Result<(), Box<dyn error::Error>> {
        let len = Qnum::encode(encoded, name_idx as u32, 6);
        let wire_len = encoded.len();
        if on_static { // "T" bit
            encoded[wire_len - len] |= 0b01000000;
        }
        encoded[wire_len - len] |= Instruction::INSERT_WITH_NAME_REFERENCE;
        // TODO: "H" bit
        let _ = Qnum::encode(encoded, value.len() as u32, 7);
        encoded.append(&mut value.as_bytes().to_vec());
        Ok(())
    }
    pub fn insert_with_literal_name(&self, encoded: &mut Vec<u8>, name: String, value: String) -> Result<(), Box<dyn error::Error>> {
        let len = Qnum::encode(encoded, name.len() as u32, 5);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= Instruction::INSERT_WITH_LITERAL_NAME;
        encoded.append(&mut name.as_bytes().to_vec());
        let _ = Qnum::encode(encoded, value.len() as u32, 7);
        encoded.append(&mut value.as_bytes().to_vec());
        Ok(())
    }
    pub fn duplicate(&self, encoded: &mut Vec<u8>, idx: usize) -> Result<(), Box<dyn error::Error>> {
        let len  = Qnum::encode(encoded, idx as u32, 5);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= Instruction::DUPLICATE;
        Ok(())
    }

    // Decode Decoder instructions
    pub fn section_ackowledgment(&mut self, wire: &Vec<u8>, idx: usize, table: &mut Table) -> Result<(usize, u16), Box<dyn error::Error>> {
        let (len, stream_id) = Qnum::decode(wire, idx, 7);

        let section = self.ack_section(stream_id as u16);
        table.ack_section(section);
        Ok((len, stream_id as u16))
    }
    pub fn stream_cancellation(&mut self, wire: &Vec<u8>, idx: usize) -> Result<(usize, u16), Box<dyn error::Error>> {
        let (len, stream_id) = Qnum::decode(wire, idx, 6);
        self.cancel_section(stream_id as u16);
        Ok((len, stream_id as u16))
    }
    pub fn insert_count_increment(&self, wire: &Vec<u8>, idx: usize) -> Result<(usize, usize), Box<dyn error::Error>> {
        let (len, increment) = Qnum::decode(wire, idx, 6);
        Ok((len, increment as usize))
    }

    pub fn prefix(&self, encoded: &mut Vec<u8>, table: &Table, required_insert_count: u32, s_flag: bool, base: u32) {
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

    pub fn indexed(&self, encoded: &mut Vec<u8>, idx: u32, from_static: bool) {
        let len = Qnum::encode(encoded, idx, 6);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= FieldType::INDEXED;
        if from_static {
            let wire_len = encoded.len();
            encoded[wire_len - len] |= 0b01000000;
        }
    }
    pub fn indexed_post_base_index(&self, encoded: &mut Vec<u8>, idx: u32) {
        let len = Qnum::encode(encoded, idx, 4);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= FieldType::INDEXED_POST_BASE_INDEX;
    }
    pub fn literal_name_reference(
        &self,
        encoded: &mut Vec<u8>,
        idx: u32,
        value: &str,
        from_static: bool,
    ) {
        // TODO: "N" bit?
        let len = Qnum::encode(encoded, idx, 4);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= FieldType::LITERAL_NAME_REFERENCE;
        if from_static {
            let wire_len = encoded.len();
            encoded[wire_len - len] |= 0b00010000;
        }
        // TODO: "H" bit?
        let _ = Qnum::encode(encoded, value.len() as u32, 7);
        encoded.append(&mut value.as_bytes().to_vec());
    }
    pub fn literal_post_base_name_reference(&self, encoded: &mut Vec<u8>, idx: u32, value: &str) {
        // TODO: "N" bit?
        let len = Qnum::encode(encoded, idx, 3);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= FieldType::LITERAL_POST_BASE_NAME_REFERENCE;
        // TODO: "H" bit?
        let _ = Qnum::encode(encoded, value.len() as u32, 7);
        encoded.append(&mut value.as_bytes().to_vec());
    }
    pub fn literal_literal_name(&self, encoded: &mut Vec<u8>, header: &Header) {
        // TODO: "N", "H" bit?
        let len = Qnum::encode(encoded, header.0.len() as u32, 3);
        let wire_len  = encoded.len();
        encoded[wire_len - len] |= FieldType::LITERAL_LITERAL_NAME;
        encoded.append(&mut header.0.as_bytes().to_vec());
        // TODO: "H" bit?
        let _ = Qnum::encode(encoded, header.1.len() as u32, 7);
        encoded.append(&mut header.1.as_bytes().to_vec());
    }
}
