use std::collections::HashMap;
use std::error;
use std::sync::{Arc, RwLock};

use crate::{FieldType, table::Table, Header};
use crate::transformer::huffman::HUFFMAN_TRANSFORMER;
use crate::transformer::qnum::Qnum;

pub struct Instruction;
impl Instruction {
    pub const SET_DYNAMIC_TABLE_CAPACITY: u8 = 0b00100000;
    pub const INSERT_REFER_NAME: u8 = 0b10000000;
    pub const INSERT_BOTH_LITERAL: u8 = 0b01000000;
    pub const DUPLICATE: u8 = 0b00000000;
}

pub struct Encoder {
    // $2.1.1.1
    _draining_idx: u32,
    pub known_sending_count: Arc<RwLock<usize>>, // TODO: requred?
    pub pending_sections: HashMap<u16, (usize, Vec<usize>)>,
}

impl Encoder {
    pub fn new() -> Self {
        Self {
            _draining_idx: 0,
            known_sending_count: Arc::new(RwLock::new(0)),
            pending_sections: HashMap::new(),
        }
    }
    pub fn add_section(&mut self, stream_id: u16, required_insert_count: usize, dynamic_table_indices: Vec<usize>) {
        self.pending_sections.insert(stream_id, (required_insert_count, dynamic_table_indices));
    }
    pub fn ack_section(&mut self, stream_id: u16) -> (usize, Vec<usize>) {
        // TOOD: remove unwrap
        let section = self.pending_sections.get(&stream_id).unwrap().clone();
        self.pending_sections.remove(&stream_id);
        section
    }
    pub fn cancel_section(&mut self, stream_id: u16) -> Vec<usize> {
        let (_, indices) = self.pending_sections.get(&stream_id).unwrap().clone();
        self.pending_sections.remove(&stream_id);
        indices
    }
    pub fn has_section(&self, stream_id: u16) -> bool {
        self.pending_sections.contains_key(&stream_id)
    }
    fn pack_string(encoded: &mut Vec<u8>, value: &str, n: u8, use_huffman: bool) -> Result<usize, Box<dyn error::Error>> {
        Ok(
            if use_huffman {
                // TODO: optimize
                let mut encoded2 = vec![];
                HUFFMAN_TRANSFORMER.encode(&mut encoded2, value)?;
                let len = Qnum::encode(encoded, encoded2.len() as u32, n);
                let wire_len = encoded.len();
                encoded[wire_len - len] |= 1 << n;
                let encoded2_len = encoded2.len();
                encoded.append(&mut encoded2);
                len + encoded2_len
            } else {
                let len = Qnum::encode(encoded, value.len() as u32, n);
                encoded.append(&mut value.as_bytes().to_vec());
                len + value.len()
            }
        )
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

    // Encode encoder instructions
    pub fn encode_set_dynamic_table_capacity(encoded: &mut Vec<u8>, cap: usize) -> Result<(), Box<dyn error::Error>> {
        let len = Qnum::encode(encoded, cap as u32, 5);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= Instruction::SET_DYNAMIC_TABLE_CAPACITY;
        Ok(())
    }
    pub fn encode_insert_refer_name(encoded: &mut Vec<u8>, on_static: bool, name_idx: usize, value: &str, use_huffman: bool) -> Result<(), Box<dyn error::Error>> {
        let len = Qnum::encode(encoded, name_idx as u32, 6);
        let wire_len = encoded.len();
        if on_static { // "T" bit
            encoded[wire_len - len] |= 0b01000000;
        }
        encoded[wire_len - len] |= Instruction::INSERT_REFER_NAME;

        Encoder::pack_string(encoded, value, 7, use_huffman)?;
        Ok(())
    }
    pub fn encode_insert_both_literal(encoded: &mut Vec<u8>, name: &str, value: &str, use_huffman: bool) -> Result<(), Box<dyn error::Error>> {
        let len = Encoder::pack_string(encoded, name, 5, use_huffman)?;
        let wire_len = encoded.len();
        encoded[wire_len - len] |= Instruction::INSERT_BOTH_LITERAL;
        Encoder::pack_string(encoded, value, 7, use_huffman)?;
        Ok(())
    }
    pub fn encode_duplicate(encoded: &mut Vec<u8>, idx: usize) -> Result<(), Box<dyn error::Error>> {
        let len  = Qnum::encode(encoded, idx as u32, 5);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= Instruction::DUPLICATE;
        Ok(())
    }

    // Decode decoder instructions
    pub fn decode_section_ackowledgment(wire: &Vec<u8>, idx: usize) -> Result<(usize, u16), Box<dyn error::Error>> {
        let (len, stream_id) = Qnum::decode(wire, idx, 7);
        Ok((len, stream_id as u16))
    }
    pub fn decode_stream_cancellation(wire: &Vec<u8>, idx: usize) -> Result<(usize, u16), Box<dyn error::Error>> {
        let (len, stream_id) = Qnum::decode(wire, idx, 6);
        Ok((len, stream_id as u16))
    }
    pub fn decode_insert_count_increment(wire: &Vec<u8>, idx: usize) -> Result<(usize, usize), Box<dyn error::Error>> {
        let (len, increment) = Qnum::decode(wire, idx, 6);
        Ok((len, increment as usize))
    }

    // Encode sending headers
    pub fn encode_indexed(encoded: &mut Vec<u8>, idx: u32, from_static: bool) {
        let len = Qnum::encode(encoded, idx, 6);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= FieldType::INDEXED;
        if from_static {
            let wire_len = encoded.len();
            encoded[wire_len - len] |= 0b01000000;
        }
    }
    pub fn encode_indexed_post_base(encoded: &mut Vec<u8>, idx: u32) {
        let len = Qnum::encode(encoded, idx, 4);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= FieldType::INDEXED_POST_BASE;
    }
    pub fn encode_refer_name(
        encoded: &mut Vec<u8>,
        idx: u32,
        value: &str,
        from_static: bool,
        use_huffman: bool
    ) -> Result<usize, Box<dyn error::Error>> {
        // TODO: "N" bit?
        let len = Qnum::encode(encoded, idx, 4);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= FieldType::REFER_NAME;
        if from_static {
            let wire_len = encoded.len();
            encoded[wire_len - len] |= 0b00010000;
        }
        Encoder::pack_string(encoded, value, 7, use_huffman)
    }
    pub fn encode_refer_name_post_base(encoded: &mut Vec<u8>, idx: u32, value: &str, use_huffman: bool)
        -> Result<usize, Box<dyn error::Error>> {
        // TODO: "N" bit?
        let len = Qnum::encode(encoded, idx, 3);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= FieldType::REFER_NAME_POST_BASE;
        Encoder::pack_string(encoded, value, 7, use_huffman)
    }
    pub fn encode_both_literal(encoded: &mut Vec<u8>, header: &Header, use_huffman: bool)
        -> Result<usize, Box<dyn error::Error>>{
        // TODO: "N"?
        let len = Encoder::pack_string(encoded, &header.0, 3, use_huffman).unwrap();
        let wire_len  = encoded.len();
        encoded[wire_len - len] |= FieldType::BOTH_LITERAL;
        Encoder::pack_string(encoded, &header.1, 7, use_huffman)
    }
}
