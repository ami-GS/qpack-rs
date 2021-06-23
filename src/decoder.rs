use std::error;

use crate::{DecompressionFailed, FieldType, Header, Qnum, table::Table};

struct Instruction;
impl Instruction {
    pub const SectionAcknowledgment: u8 = 0b10000000;
    pub const StreamCancellation: u8 = 0b01000000;
    pub const InsertCountIncrement: u8 = 0b00000000;
}

pub struct Decoder {
    size: usize,
    known_received_count: u32,
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            size: 0,
            known_received_count: 0,
        }
    }
    pub fn decode<'a>(&'a self, wire: &'a Vec<u8>, table: &mut Table) -> Result<Vec<Header>, Box<dyn error::Error>> {
        let mut idx = 0;
        let (len, encoded_insert_count, delta) = self.prefix(wire, idx, table);
        idx += len;
        // let mut headers: Vec<Header<'a>> = Vec::<Header<'a>>::new();
        // let mut headers: Vec<Header<'a>> = vec![];
        let mut headers = vec![];
        let wire_len = wire.len();
        while idx < wire_len {
            let ret = if wire[idx] & FieldType::Indexed == FieldType::Indexed {
                self.indexed(wire, idx, table)
            } else if wire[idx] & FieldType::LiteralNameReference == FieldType::LiteralNameReference {
                self.literal_name_reference(wire, idx, table)
            } else if wire[idx] & FieldType::LiteralLiteralName == FieldType::LiteralLiteralName {
                self.literal_literal_name(wire, idx, table)
            } else if wire[idx] & FieldType::IndexedPostBaseIndex == FieldType::IndexedPostBaseIndex {
                self.indexed_post_base_index(wire, idx, table)
            } else if wire[idx] & 0b11110000 == FieldType::LiteralPostBaseNameReference {
                self.literal_post_base_name_reference(wire, idx, table)
            } else {
                return Err(DecompressionFailed.into());
            };
            idx += ret.0;
            headers.push(ret.1);
        }
        Ok(headers)
    }

    fn prefix(&self, wire: &Vec<u8>, base: usize, table: &Table) -> (usize, u32, u32) {
        let (len1, required_insert_count) = Qnum::decode(wire, base, 8);
        let encoded_insert_count = if required_insert_count == 0 {
            0
        } else {
            required_insert_count % (2 * table.get_max_entries()) + 1
        };

        let s_flag = (wire[base + len1] & 0b10000000) == 0b10000000;
        let (len2, delta_base) = Qnum::decode(wire, base + len1, 7);
        let delta = if s_flag {
            required_insert_count - delta_base - 1
        } else {
            required_insert_count + delta_base
        };

        (len1 + len2, encoded_insert_count, delta)
    }

    fn indexed(&self, wire: &Vec<u8>, idx: usize, table: &Table) -> (usize, Header) {
        let (len, table_idx) = Qnum::decode(wire, idx, 6);

        if wire[idx] & 0b01000000 == 0b01000000 {
            (len, table.get_from_static(table_idx as usize))
        } else {
            // TODO: should not correct indexing
            (len, table.get_from_dynamic(table_idx as usize))
        }
    }

    fn literal_name_reference<'a>(&self, wire: &'a Vec<u8>, idx: usize, table: &Table) -> (usize, Header<'a>) {
        let (len1, table_idx) = Qnum::decode(wire, idx, 4);

        let header = if wire[idx] & 0b00010000 == 0b00010000 {
            table.get_from_static(table_idx as usize)
        } else {
            table.get_from_dynamic(table_idx as usize)
        };
        let (len2, value_length) = Qnum::decode(wire, idx + len1, 7);
        wire[idx + len1 + len2];
        let value = std::str::from_utf8(
            &wire[(idx + len1 + len2)..(idx + len1 + len2 + value_length as usize)],
        )
        .unwrap();
        (len1 + len2 + value_length as usize, (header.0, value))
    }

    fn literal_literal_name<'a>(&self, wire: &'a Vec<u8>, idx: usize, _table: &Table) -> (usize, Header<'a>) {
        let (len1, name_length) = Qnum::decode(wire, idx, 3);
        let name = std::str::from_utf8(
            &wire[(idx + len1)..(idx + len1 + name_length as usize)],
        )
        .unwrap();
        let (len2, value_length) = Qnum::decode(wire, idx + len1 + name_length as usize, 7);
        let value = std::str::from_utf8(
            &wire[(idx + len1 + len2 + name_length as usize)..(idx + len1 + len2 + name_length as usize + value_length as usize)],
        )
        .unwrap();
        (len1 + len2 + name_length as usize + value_length as usize, (name, value))
    }

    fn indexed_post_base_index(&self, wire: &Vec<u8>, idx: usize, table: &Table) -> (usize, Header) {
        let (len1, table_idx) = Qnum::decode(wire, idx, 4);
        // TODO: should not correct indexing
        let header = table.get_from_dynamic(table_idx as usize);
        (len1, header)
    }

    fn literal_post_base_name_reference<'a>(&self, wire: &'a Vec<u8>, idx: usize, table: &Table) -> (usize, Header<'a>) {
        let (len1, table_idx) = Qnum::decode(wire, idx, 3);
        let header = table.get_from_dynamic(table_idx as usize);
        let (len2, value_length) = Qnum::decode(wire, idx + len1, 7);
        let value = std::str::from_utf8(
            &wire[(idx + len1)..(idx + len1 + value_length as usize)],
        )
        .unwrap();
        (len1 + len2 + value_length as usize, (header.0, value))
    }
}
