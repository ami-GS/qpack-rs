use std::{collections::HashMap, error};

use crate::types::HeaderString;
use crate::{DecompressionFailed, Header, table::Table};
use crate::transformer::huffman::HUFFMAN_TRANSFORMER;
use crate::transformer::qnum::Qnum;

pub struct Instruction;
impl Instruction {
    pub const SECTION_ACKNOWLEDGMENT: u8 = 0b10000000;
    pub const STREAM_CANCELLATION: u8 = 0b01000000;
    pub const _INSERT_COUNT_INCREMENT: u8 = 0b00000000;
}

pub struct Decoder {
    pub current_blocked_streams: u16,
    pub pending_sections: HashMap<u16, usize>,
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            current_blocked_streams: 0,
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
    fn parse_string(wire: &Vec<u8>, idx: usize, n: u8) -> Result<(usize, HeaderString), Box<dyn error::Error>> {
        let (len, value_len) = Qnum::decode(wire, idx, n);
        Ok((len + value_len as usize,
        if wire[idx] & (1 << n) > 0 {
            HeaderString::new(HUFFMAN_TRANSFORMER.decode(wire, idx + len, value_len as usize)?, true)
        } else {
            HeaderString::new(std::str::from_utf8(
                &wire[(idx + len)..(idx + len + value_len as usize)],
            )?.to_string(), false)
        }))
    }
    pub fn prefix(wire: &Vec<u8>, idx: usize, table: &Table) -> Result<(usize, u32, usize), Box<dyn error::Error>> {
        let (len1, encoded_insert_count) = Qnum::decode(wire, idx, 8);

        // # 4.5.1.1
        let required_insert_count = if encoded_insert_count == 0 {
            0
        } else {
            let max_entries = table.get_max_entries();
            let total_number_of_inserts = table.get_insert_count();
            let full_range = 2 * max_entries;
            if encoded_insert_count > full_range {
                return Err(DecompressionFailed.into());
            }
            let max_value = total_number_of_inserts as u32 + max_entries;
            let max_wrapped = ((max_value as f64 / full_range as f64).floor() as u32) * full_range;
            let mut requred_insert_count = max_wrapped + encoded_insert_count - 1;
            if requred_insert_count > max_value {
                if requred_insert_count <= full_range {
                    return Err(DecompressionFailed.into());
                }
                requred_insert_count -= full_range;
            }
            if requred_insert_count == 0 {
                return Err(DecompressionFailed.into());
            }
            requred_insert_count
        };

        let s_flag = (wire[idx + len1] & 0b10000000) == 0b10000000;
        let (len2, delta_base) = Qnum::decode(wire, idx + len1, 7);
        let base = if s_flag {
            required_insert_count - delta_base - 1
        } else {
            required_insert_count + delta_base
        };

        Ok((len1 + len2, required_insert_count, base as usize))
    }

    // Encode decoder instructions
    pub fn encode_section_ackowledgment(encoded: &mut Vec<u8>, stream_id: u16) -> Result<(), Box<dyn error::Error>> {
        // TODO: double check streamID's max length
        let len = Qnum::encode(encoded, stream_id as u32, 7);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= Instruction::SECTION_ACKNOWLEDGMENT;
        Ok(())
    }
    pub fn encode_stream_cancellation(encoded: &mut Vec<u8>, stream_id: u16) -> Result<(), Box<dyn error::Error>> {
        // TODO: double check streamID's max length
        let len = Qnum::encode(encoded, stream_id as u32, 6);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= Instruction::STREAM_CANCELLATION;
        Ok(())
    }
    pub fn encode_insert_count_increment(encoded: &mut Vec<u8>, increment: usize) -> Result<(), Box<dyn error::Error>> {
        let _ = Qnum::encode(encoded, increment as u32, 6);
        Ok(())
    }

    // Decode encoder instructions
    pub fn decode_dynamic_table_capacity(wire: &Vec<u8>, idx: usize) -> Result<(usize, usize), Box<dyn error::Error>> {
        let (len1, cap) = Qnum::decode(wire, idx, 5);
        Ok((len1, cap as usize))
    }
    pub fn decode_insert_refer_name(wire: &Vec<u8>, idx: usize) -> Result<(usize, (usize, HeaderString, bool)), Box<dyn error::Error>> {
        let on_static_table = wire[idx] & 0b01000000 == 0b01000000;
        let (len1, name_idx) = Qnum::decode(wire, idx, 6);
        let (len2, value) = Decoder::parse_string(wire, idx + len1, 7)?;
        Ok((len1 + len2, (name_idx as usize, value, on_static_table)))
    }
    pub fn decode_insert_both_literal(wire: &Vec<u8>, idx: usize) -> Result<(usize, Header), Box<dyn error::Error>> {
        let (len1, name) = Decoder::parse_string(wire, idx, 5)?;
        let (len2, value) = Decoder::parse_string(wire, idx + len1, 7)?;
        Ok((len1 + len2, Header::new_with_header_string(name, value, false)))
    }
    pub fn decode_duplicate(wire: &Vec<u8>, idx: usize) -> Result<(usize, usize), Box<dyn error::Error>> {
        let (len, index) = Qnum::decode(wire, idx, 5);
        Ok((len, index as usize))
    }

    // Decode received headers
    pub fn decode_indexed(wire: &Vec<u8>, idx: &mut usize, base: usize, required_insert_count: usize, table: &Table) -> Result<(Header, bool), Box<dyn error::Error>> {
        let from_static = wire[*idx] & 0b01000000 == 0b01000000;
        let (len, table_idx) = Qnum::decode(wire, *idx, 6);
        *idx += len;

        let table_idx = table_idx as usize;
        Ok(
            if from_static {
                (table.get_header_from_static(table_idx)?, false)
            } else {
                if required_insert_count <= table_idx {
                    return Err(DecompressionFailed.into());
                }
                (table.get_header_from_dynamic(base, table_idx, false)?, true)
            }
        )
    }
    pub fn decode_refer_name(wire: &Vec<u8>, idx: &mut usize, base: usize, required_insert_count: usize, table: &Table) -> Result<(Header, bool), Box<dyn error::Error>> {
        let (len, table_idx) = Qnum::decode(wire, *idx, 4);
        let from_static = wire[*idx] & 0b00010000 == 0b00010000;
        let is_sensitive = wire[*idx] & 0b00100000 == 0b00100000;
        *idx += len;

        let table_idx = table_idx as usize;
        let mut header = if from_static {
            table.get_header_from_static(table_idx)?
        } else {
            if required_insert_count <= table_idx {
                return Err(DecompressionFailed.into());
            }
            table.get_header_from_dynamic(base, table_idx, false)?
        };
        let (len, value) = Decoder::parse_string(wire, *idx, 7)?;
        *idx += len;
        header.set_value(value);
        header.set_sensitive(is_sensitive);
        Ok((header, !from_static))
    }
    pub fn decode_both_literal(wire: &Vec<u8>, idx: &mut usize) -> Result<(Header, bool), Box<dyn error::Error>> {
        let is_sensitive = wire[*idx] & 0b00010000 == 0b00010000;
        let (len, name) = Decoder::parse_string(wire, *idx, 3)?;
        *idx += len;
        let (len, value) = Decoder::parse_string(wire, *idx, 7)?;
        *idx += len;

        Ok((Header::new_with_header_string(name, value, is_sensitive), false))
    }
    pub fn decode_indexed_post_base(wire: &Vec<u8>, idx: &mut usize, base: usize, required_insert_count: usize, table: &Table) -> Result<(Header, bool), Box<dyn error::Error>> {
        let (len, table_idx) = Qnum::decode(wire, *idx, 4);
        let table_idx = table_idx as usize;
        if required_insert_count <= table_idx {
            return Err(DecompressionFailed.into());
        }
        *idx += len;
        let header = table.get_header_from_dynamic(base, table_idx, true)?;
        Ok((header, true))
    }
    pub fn decode_refer_name_post_base(wire: &Vec<u8>, idx: &mut usize, base: usize, required_insert_count: usize, table: &Table) -> Result<(Header, bool), Box<dyn error::Error>> {
        let is_sensitive = wire[*idx] & 0b00001000 == 0b00001000;
        let (len, table_idx) = Qnum::decode(wire, *idx, 3);
        let table_idx = table_idx as usize;
        if required_insert_count <= table_idx {
            return Err(DecompressionFailed.into());
        }
        *idx += len;
        let mut header = table.get_header_from_dynamic(base, table_idx, true)?;
        let (len, value) = Decoder::parse_string(wire, *idx, 7)?;
        *idx += len;
        header.set_sensitive(is_sensitive);
        header.set_value(value);
        Ok((header, true))
    }
}
