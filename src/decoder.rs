use std::{collections::HashMap, error};

use crate::{DecompressionFailed, Header, Qnum, table::Table};

pub struct Instruction;
impl Instruction {
    pub const SECTION_ACKNOWLEDGMENT: u8 = 0b10000000;
    pub const STREAM_CANCELLATION: u8 = 0b01000000;
    pub const _INSERT_COUNT_INCREMENT: u8 = 0b00000000;
}

pub struct Decoder {
    _size: usize,
    pub pending_sections: HashMap<u16, usize>, // experimental
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            _size: 0,
            pending_sections: HashMap::new(),
        }
    }
    // Decode Encoder instructions
    pub fn dynamic_table_capacity(&self, wire: &Vec<u8>, idx: usize) -> Result<(usize, usize), Box<dyn error::Error>> {
        let (len1, cap) = Qnum::decode(wire, idx, 5);
        Ok((len1, cap as usize))
    }
    pub fn insert_with_name_reference(&self, wire: &Vec<u8>, idx: usize) -> Result<(usize, (usize, String, bool)), Box<dyn error::Error>> {
        let on_static_table = wire[idx] & 0b01000000 == 0b01000000;
        let (len1, name_idx) = Qnum::decode(wire, idx, 6);
        let (len2, value_len) = Qnum::decode(wire, idx + len1, 7);
        let value = std::str::from_utf8(
            &wire[(idx + len1 + len2)..(idx + len1 + len2 + value_len as usize)],
        )?;
        // TODO: check "H" bit
        Ok((len1 + len2 + value_len as usize, (name_idx as usize, value.to_string(), on_static_table)))
    }
    pub fn insert_with_literal_name(&self, wire: &Vec<u8>, idx: usize) -> Result<(usize, (String, String)), Box<dyn error::Error>> {
        // TODO: check "H" bits
        let (len1, name_len) = Qnum::decode(wire, idx, 5);
        let name = std::str::from_utf8(&wire[(idx + len1)..(idx + len1 + name_len as usize)])?;

        let (len2, value_len) = Qnum::decode(wire, idx + len1 + name_len as usize, 7);
        let value = std::str::from_utf8(
            &wire[(idx + len1 + len2 + name_len as usize)..(idx + len1 + len2 + name_len as usize + value_len as usize)],
        )?;
        Ok((len1 + len2 + name_len as usize + value_len as usize, (name.to_string(), value.to_string())))
    }
    pub fn duplicate(&self, wire: &Vec<u8>, idx: usize) -> Result<(usize, usize), Box<dyn error::Error>> {
        let (len, index) = Qnum::decode(wire, idx, 5);
        Ok((len, index as usize))
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
    // Encode Decoder instructions
    pub fn section_ackowledgment(&self, encoded: &mut Vec<u8>, stream_id: u16) -> Result<(), Box<dyn error::Error>> {
        // TODO: double check streamID's max length
        let len = Qnum::encode(encoded, stream_id as u32, 7);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= Instruction::SECTION_ACKNOWLEDGMENT;
        Ok(())
    }
    pub fn stream_cancellation(&self, encoded: &mut Vec<u8>, stream_id: u16) -> Result<(), Box<dyn error::Error>> {
        // TODO: double check streamID's max length
        let len = Qnum::encode(encoded, stream_id as u32, 6);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= Instruction::STREAM_CANCELLATION;
        Ok(())
    }
    pub fn insert_count_increment(&self, encoded: &mut Vec<u8>, increment: usize) -> Result<(), Box<dyn error::Error>> {
        let _ = Qnum::encode(encoded, increment as u32, 6);
        Ok(())
    }

    pub fn prefix(&self, wire: &Vec<u8>, idx: usize, table: &Table) -> Result<(usize, u32, usize), Box<dyn error::Error>> {
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

    pub fn indexed(&self, wire: &Vec<u8>, idx: &mut usize, base: usize, table: &Table) -> Result<(Header, bool), Box<dyn error::Error>> {
        let from_static = wire[*idx] & 0b01000000 == 0b01000000;
        let (len, table_idx) = Qnum::decode(wire, *idx, 6);
        *idx += len;

        Ok(
            if from_static {
                (table.get_from_static(table_idx as usize)?, false)
            } else {
                (table.get_from_dynamic(base, table_idx as usize, false)?, true)
            }
        )
    }

    pub fn literal_name_reference(&self, wire: &Vec<u8>, idx: &mut usize, base: usize, table: &Table) -> Result<(Header, bool), Box<dyn error::Error>> {
        let (len, table_idx) = Qnum::decode(wire, *idx, 4);
        let from_static = wire[*idx] & 0b00010000 == 0b00010000;
        *idx += len;

        let header = if from_static {
            table.get_from_static(table_idx as usize)?
        } else {
            table.get_from_dynamic(base, table_idx as usize, false)?
        };
        let (len, value_length) = Qnum::decode(wire, *idx, 7);
        *idx += len;
        let value = std::str::from_utf8(
            &wire[*idx..*idx + value_length as usize],
        )?;
        *idx += value_length  as usize;

        Ok((Header::from_string(header.0, value.to_string()), !from_static))
    }

    pub fn literal_literal_name(&self, wire: &Vec<u8>, idx: &mut usize) -> Result<(Header, bool), Box<dyn error::Error>> {
        let (len, name_length) = Qnum::decode(wire, *idx, 3);
        *idx += len;
        let name = std::str::from_utf8(
            &wire[*idx..*idx + name_length as usize],
        )?;
        *idx += name_length as usize;
        let (len, value_length) = Qnum::decode(wire, *idx, 7);
        *idx += len;
        let value = std::str::from_utf8(
            &wire[*idx..*idx + value_length as usize],
        )?;
        *idx += value_length as usize;
        Ok((Header::from(name, value), false))
    }

    pub fn indexed_post_base_index(&self, wire: &Vec<u8>, idx: &mut usize, base: usize, table: &Table) -> Result<(Header, bool), Box<dyn error::Error>> {
        let (len, table_idx) = Qnum::decode(wire, *idx, 4);
        *idx += len;
        let header = table.get_from_dynamic(base, table_idx as usize, true)?;
        Ok((header, true))
    }

    pub fn literal_post_base_name_reference(&self, wire: &Vec<u8>, idx: &mut usize, base: usize, table: &Table) -> Result<(Header, bool), Box<dyn error::Error>> {
        let (len, table_idx) = Qnum::decode(wire, *idx, 3);
        *idx += len;
        let header = table.get_from_dynamic(base, table_idx as usize, true)?;
        let (len, value_length) = Qnum::decode(wire, *idx, 7);
        *idx += len;
        let value = std::str::from_utf8(
            &wire[*idx..*idx + value_length as usize],
        )?;
        *idx += value_length as usize;
        Ok((Header::from_string(header.0, value.to_string()), true))
    }
}
