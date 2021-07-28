use std::collections::HashMap;
use std::error;
use std::sync::{Arc, RwLock};

use crate::huffman::HuffmanTransformer;
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
    pub known_sending_count: Arc<RwLock<usize>>, // TODO: requred?
    pub pending_sections: HashMap<u16, usize>, // experimental
}

impl Encoder {
    pub fn new() -> Self {
        Self {
            _draining_idx: 0,
            known_sending_count: Arc::new(RwLock::new(0)),
            pending_sections: HashMap::new(),
        }
    }
    pub fn add_section(&mut self, stream_id: u16, required_insert_count: usize) {
        self.pending_sections.insert(stream_id, required_insert_count);
    }
    pub fn ack_section(&mut self, stream_id: u16) -> usize {
        // TOOD: remove unwrap
        let section = self.pending_sections.get(&stream_id).unwrap().clone();
        self.pending_sections.remove(&stream_id);
        section
    }
    pub fn cancel_section(&mut self, stream_id: u16) {
        self.pending_sections.remove(&stream_id);
    }
    pub fn has_section(&self, stream_id: u16) -> bool {
        self.pending_sections.contains_key(&stream_id)
    }
    // Encode Encoder instructions
    // WARN: confusing name
    pub fn set_dynamic_table_capacity(encoded: &mut Vec<u8>, cap: usize) -> Result<(), Box<dyn error::Error>> {
        let len = Qnum::encode(encoded, cap as u32, 5);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= Instruction::SET_DYNAMIC_TABLE_CAPACITY;
        Ok(())
    }
    fn pack_string(encoded: &mut Vec<u8>, value: &str, n: u8, huffman_transformer: Option<&HuffmanTransformer>) -> Result<usize, Box<dyn error::Error>> {
        Ok(
            if let Some(transformer) = huffman_transformer {
                // TODO: optimize
                let mut encoded2 = vec![];
                transformer.encode(&mut encoded2, value)?;
                let len = Qnum::encode(encoded, encoded2.len() as u32, n);
                let wire_len = encoded.len();
                encoded[wire_len - len] |= 1 << n;
                encoded.append(&mut encoded2);
                len + encoded2.len()
            } else {
                let len = Qnum::encode(encoded, value.len() as u32, n);
                encoded.append(&mut value.as_bytes().to_vec());
                len + value.len()
            }
        )
    }
    pub fn insert_with_name_reference(encoded: &mut Vec<u8>, on_static: bool, name_idx: usize, value: &str, huffman_transformer: Option<&HuffmanTransformer>) -> Result<(), Box<dyn error::Error>> {
        let len = Qnum::encode(encoded, name_idx as u32, 6);
        let wire_len = encoded.len();
        if on_static { // "T" bit
            encoded[wire_len - len] |= 0b01000000;
        }
        encoded[wire_len - len] |= Instruction::INSERT_WITH_NAME_REFERENCE;

        Encoder::pack_string(encoded, value, 7, huffman_transformer)?;
        Ok(())
    }
    pub fn insert_with_literal_name(encoded: &mut Vec<u8>, name: &str, value: &str, huffman_transformer: Option<&HuffmanTransformer>) -> Result<(), Box<dyn error::Error>> {
        let len = Encoder::pack_string(encoded, name, 5, huffman_transformer)?;
        let wire_len = encoded.len();
        encoded[wire_len - len] |= Instruction::INSERT_WITH_LITERAL_NAME;
        Encoder::pack_string(encoded, value, 7, huffman_transformer)?;
        Ok(())
    }
    pub fn duplicate(encoded: &mut Vec<u8>, idx: usize) -> Result<(), Box<dyn error::Error>> {
        let len  = Qnum::encode(encoded, idx as u32, 5);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= Instruction::DUPLICATE;
        Ok(())
    }

    // Decode Decoder instructions
    pub fn section_ackowledgment(wire: &Vec<u8>, idx: usize) -> Result<(usize, u16), Box<dyn error::Error>> {
        let (len, stream_id) = Qnum::decode(wire, idx, 7);
        Ok((len, stream_id as u16))
    }
    pub fn stream_cancellation(wire: &Vec<u8>, idx: usize) -> Result<(usize, u16), Box<dyn error::Error>> {
        let (len, stream_id) = Qnum::decode(wire, idx, 6);
        Ok((len, stream_id as u16))
    }
    pub fn insert_count_increment(wire: &Vec<u8>, idx: usize) -> Result<(usize, usize), Box<dyn error::Error>> {
        let (len, increment) = Qnum::decode(wire, idx, 6);
        Ok((len, increment as usize))
    }

    pub fn prefix(encoded: &mut Vec<u8>, table: &Table, required_insert_count: u32, s_flag: bool, base: u32) {
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

    pub fn indexed(encoded: &mut Vec<u8>, idx: u32, from_static: bool) {
        let len = Qnum::encode(encoded, idx, 6);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= FieldType::INDEXED;
        if from_static {
            let wire_len = encoded.len();
            encoded[wire_len - len] |= 0b01000000;
        }
    }
    pub fn indexed_post_base_index(encoded: &mut Vec<u8>, idx: u32) {
        let len = Qnum::encode(encoded, idx, 4);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= FieldType::INDEXED_POST_BASE_INDEX;
    }
    pub fn literal_name_reference(
        encoded: &mut Vec<u8>,
        idx: u32,
        value: &str,
        from_static: bool,
        huffman_transformer: Option<&HuffmanTransformer>
    ) {
        // TODO: "N" bit?
        let len = Qnum::encode(encoded, idx, 4);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= FieldType::LITERAL_NAME_REFERENCE;
        if from_static {
            let wire_len = encoded.len();
            encoded[wire_len - len] |= 0b00010000;
        }
        // TODO: error handling
        Encoder::pack_string(encoded, value, 7, huffman_transformer);
    }
    pub fn literal_post_base_name_reference(encoded: &mut Vec<u8>, idx: u32, value: &str, huffman_transformer: Option<&HuffmanTransformer>) {
        // TODO: "N" bit?
        let len = Qnum::encode(encoded, idx, 3);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= FieldType::LITERAL_POST_BASE_NAME_REFERENCE;
        Encoder::pack_string(encoded, value, 7, huffman_transformer);
    }
    pub fn literal_literal_name(encoded: &mut Vec<u8>, header: &Header, huffman_transformer: Option<&HuffmanTransformer>) {
        // TODO: "N"?
        let len = Encoder::pack_string(encoded, &header.0, 3, huffman_transformer).unwrap();
        let wire_len  = encoded.len();
        encoded[wire_len - len] |= FieldType::LITERAL_LITERAL_NAME;
        Encoder::pack_string(encoded, &header.1, 7, huffman_transformer);
    }
}
