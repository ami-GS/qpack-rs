use std::{collections::HashMap, error};

use crate::{DecompressionFailed, Header, Qnum, table::Table};

pub struct Instruction;
impl Instruction {
    pub const SectionAcknowledgment: u8 = 0b10000000;
    pub const StreamCancellation: u8 = 0b01000000;
    pub const InsertCountIncrement: u8 = 0b00000000;
}

pub struct Decoder {
    size: usize,
    pub known_received_count: u32,
    pub pending_sections: HashMap<u16, usize>, // experimental
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            size: 0,
            known_received_count: 0,
            pending_sections: HashMap::new(),
        }
    }
    pub fn get_known_received_count(&self) -> u32 {
        self.known_received_count
    }

    // Decode Encoder instructions
    pub fn dynamic_table_capacity(&self, wire: &Vec<u8>, idx: usize, table: &mut Table) -> Result<usize, Box<dyn error::Error>> {
        let (len1, cap) = Qnum::decode(wire, idx, 5);
        table.set_dynamic_table_capacity(cap as usize)?;
        Ok(len1)
    }
    pub fn insert_with_name_reference(&self, wire: &Vec<u8>, idx: usize, table: &mut Table) -> Result<usize, Box<dyn error::Error>> {
        let on_static_table = wire[idx] & 0b01000000 == 0b01000000;
        let (len1, name_idx) = Qnum::decode(wire, idx, 6);
        let (len2, value_len) = Qnum::decode(wire, idx + len1, 7);
        let value = std::str::from_utf8(
            &wire[(idx + len1 + len2)..(idx + len1 + len2 + value_len as usize)],
        )?;
        // TODO: check "H" bit
        table.insert_with_name_reference(name_idx as usize, value.to_string(), on_static_table)?;
        Ok(len1 + len2 + value_len as usize)
    }
    pub fn insert_with_literal_name(&self, wire: &Vec<u8>, idx: usize, table: &mut Table) -> Result<usize, Box<dyn error::Error>> {
        // TODO: check "H" bits
        let (len1, name_len) = Qnum::decode(wire, idx, 5);
        let name = std::str::from_utf8(&wire[(idx + len1)..(idx + len1 + name_len as usize)])?;

        let (len2, value_len) = Qnum::decode(wire, idx + len1 + name_len as usize, 7);
        let value = std::str::from_utf8(
            &wire[(idx + len1 + len2 + name_len as usize)..(idx + len1 + len2 + name_len as usize + value_len as usize)],
        )?;
        table.insert_with_literal_name(name.to_string(), value.to_string())?;
        Ok(len1 + len2 + name_len as usize + value_len as usize)
    }
    pub fn duplicate(&self, wire: &Vec<u8>, idx: usize, table: &mut Table) -> Result<usize, Box<dyn error::Error>> {
        let (len, index) = Qnum::decode(wire, idx, 5);
        table.duplicate(index as usize)?;
        Ok(len)
    }

    // Encode Decoder instructions
    pub fn section_ackowledgment(&self, encoded: &mut Vec<u8>, table: &mut Table, stream_id: u16) -> Result<(), Box<dyn error::Error>> {
        // TODO: double check streamID's max length
        let len = Qnum::encode(encoded, stream_id as u32, 7);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= Instruction::SectionAcknowledgment;

        let section = self.pending_sections.get(&stream_id).unwrap();
        (*table.dynamic_table).borrow_mut().ack_section(*section);
        Ok(())
    }
    pub fn stream_cancellation(&self, encoded: &mut Vec<u8>, stream_id: u16) -> Result<(), Box<dyn error::Error>> {
        // TODO: double check streamID's max length
        let len = Qnum::encode(encoded, stream_id as u32, 6);
        let wire_len = encoded.len();
        encoded[wire_len - len] |= Instruction::StreamCancellation;
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

    pub fn indexed(&self, wire: &Vec<u8>, idx: usize, base: usize, table: &Table) -> Result<(usize, Header), Box<dyn error::Error>> {
        let (len, table_idx) = Qnum::decode(wire, idx, 6);

        Ok(
            if wire[idx] & 0b01000000 == 0b01000000 {
                (len, table.get_from_static(table_idx as usize)?)
            } else {
                (len, table.get_from_dynamic(base, table_idx as usize, false)?)
            }
        )
    }

    pub fn literal_name_reference(&self, wire: &Vec<u8>, idx: usize, base: usize, table: &Table) -> Result<(usize, Header), Box<dyn error::Error>> {
        let (len1, table_idx) = Qnum::decode(wire, idx, 4);

        let header = if wire[idx] & 0b00010000 == 0b00010000 {
            table.get_from_static(table_idx as usize)?
        } else {
            table.get_from_dynamic(base, table_idx as usize, false)?
        };
        let (len2, value_length) = Qnum::decode(wire, idx + len1, 7);
        wire[idx + len1 + len2];
        let value = std::str::from_utf8(
            &wire[(idx + len1 + len2)..(idx + len1 + len2 + value_length as usize)],
        )?;

        Ok((len1 + len2 + value_length as usize, Header::from_string(header.0, value.to_string())))
    }

    pub fn literal_literal_name(&self, wire: &Vec<u8>, idx: usize) -> Result<(usize, Header), Box<dyn error::Error>> {
        let (len1, name_length) = Qnum::decode(wire, idx, 3);
        let name = std::str::from_utf8(
            &wire[(idx + len1)..(idx + len1 + name_length as usize)],
        )?;
        let (len2, value_length) = Qnum::decode(wire, idx + len1 + name_length as usize, 7);
        let value = std::str::from_utf8(
            &wire[(idx + len1 + len2 + name_length as usize)..(idx + len1 + len2 + name_length as usize + value_length as usize)],
        )?;
        Ok((len1 + len2 + name_length as usize + value_length as usize, Header::from(name, value)))
    }

    pub fn indexed_post_base_index(&self, wire: &Vec<u8>, idx: usize, base: usize, table: &Table) -> Result<(usize, Header), Box<dyn error::Error>> {
        let (len1, table_idx) = Qnum::decode(wire, idx, 4);
        let header = table.get_from_dynamic(base, table_idx as usize, true)?;
        Ok((len1, header))
    }

    pub fn literal_post_base_name_reference(&self, wire: &Vec<u8>, idx: usize, base: usize, table: &Table) -> Result<(usize, Header), Box<dyn error::Error>> {
        let (len1, table_idx) = Qnum::decode(wire, idx, 3);
        let header = table.get_from_dynamic(base, table_idx as usize, true)?;
        let (len2, value_length) = Qnum::decode(wire, idx + len1, 7);
        let value = std::str::from_utf8(
            &wire[(idx + len1)..(idx + len1 + value_length as usize)],
        )?;
        Ok((len1 + len2 + value_length as usize, Header::from_string(header.0, value.to_string())))
    }
}
