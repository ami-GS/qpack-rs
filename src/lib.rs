use crate::decoder::Decoder;
use crate::encoder::Encoder;
use crate::table::Table;
use core::fmt;
use std::error;

mod decoder;
mod dynamic_table;
mod encoder;
mod table;

pub struct Qpack {
    encoder: Encoder,
    decoder: Decoder,
    table: Table,
}

impl Qpack {
    pub fn new(dynamic_table_max_capacity: usize) -> Self {
        Qpack {
            encoder: Encoder::new(),
            decoder: Decoder::new(),
            table: Table::new(dynamic_table_max_capacity),
        }
    }
    pub fn encode_insert_header(&mut self, encoded: &mut Vec<u8>, header: Header) -> Result<(), Box<dyn error::Error>> {
        let (both_match, on_static, idx) = self.table.find_index(&header);
        let idx = if idx != (1 << 32) - 1 && !on_static {
            // absolute to relative conversion
            (*self.table.dynamic_read).borrow().insert_count - 1 - idx
        } else { idx };

        let out = if both_match && !on_static {
            self.encoder.duplicate(encoded, idx, &mut self.table)?
        } else if idx != (1 << 32) - 1 {
            self.encoder.insert_with_name_reference(encoded, on_static, idx, header.1, &mut self.table)?
        } else {
            self.encoder.insert_with_literal_name(encoded, header.0, header.1, &mut self.table)?
        };
        self.encoder.known_sending_count += 1;
        Ok(out)
    }
    pub fn encode_set_dynamic_table_capacity(&mut self, encoded: &mut Vec<u8>, capacity: u32) -> Result<(), Box<dyn error::Error>> {
        self.encoder.set_dynamic_table_capacity(encoded, capacity, &mut self.table)
    }
    pub fn encode_section_ackowledgment(&mut self, encoded: &mut Vec<u8>, stream_id: u16) -> Result<(), Box<dyn error::Error>> {
        self.decoder.section_ackowledgment(encoded, &mut self.table, stream_id)
    }
    pub fn encode_stream_cancellation(&mut self, encoded: &mut Vec<u8>, stream_id: u16) -> Result<(), Box<dyn error::Error>> {
        self.decoder.stream_cancellation(encoded, stream_id)
    }
    pub fn encode_insert_count_increment(&self, encoded: &mut Vec<u8>) -> Result<(), Box<dyn error::Error>> {
        let increment = (*self.table.dynamic_read).borrow().list.len() - (*self.table.dynamic_read).borrow().acked_section;
        let out = self.decoder.insert_count_increment(encoded, increment)?;
        (*self.table.dynamic_table).borrow_mut().acked_section += increment;
        Ok(out)
    }
    pub fn encode_headers(&mut self, encoded: &mut Vec<u8>, relative_indexing: bool, headers: Vec<Header>, stream_id: u16) -> Result<(), Box<dyn error::Error>> {
        // TODO: decide whether to use s_flag (relative indexing)
        let required_insert_count = self.table.get_insert_count();
        // TODO: suspicious
        let base = if relative_indexing {
            (*self.table.dynamic_read).borrow().acked_section as u32
        } else {
            (*self.table.dynamic_read).borrow().insert_count as u32
        };
        self.encoder.prefix(encoded, &self.table, required_insert_count as u32, relative_indexing, base);

        for header in headers {
            let (both_match, on_static, idx) = self.table.find_index(&header);
            if both_match {
                if relative_indexing {
                    self.encoder.indexed_post_base_index(encoded, idx as u32);
                } else {
                    let abs_idx = if on_static { idx } else { base as usize - idx - 1 };
                    self.encoder.indexed(encoded, abs_idx as u32, on_static);
                }
            } else if idx != (1 << 32) - 1 {
                if relative_indexing {
                    self.encoder.literal_post_base_name_reference(encoded, idx as u32, &header.1);
                } {
                    self.encoder.literal_name_reference(encoded, idx as u32, &header.1, on_static);
                }
            } else { // not found
                self.encoder.literal_literal_name(encoded, &header);
            }
        }
        self.encoder.pending_sections.insert(stream_id, required_insert_count);
        Ok(())
    }
    pub fn decode_headers(&mut self, wire: &Vec<u8>, stream_id: u16) -> Result<Vec<Header>, Box<dyn error::Error>> {
        let mut idx = 0;
        let (len, requred_insert_count, base) = self.decoder.prefix(wire, idx, &self.table)?;
        idx += len;

        // blocked if dynamic_table.insert_count < requred_insert_count
        // blocked just before referencing dynamic_table is better?

        let mut headers = vec![];
        let wire_len = wire.len();
        while idx < wire_len {
            let ret = if wire[idx] & FieldType::INDEXED == FieldType::INDEXED {
                self.decoder.indexed(wire, idx, base, &self.table)?
            } else if wire[idx] & FieldType::LITERAL_NAME_REFERENCE == FieldType::LITERAL_NAME_REFERENCE {
                self.decoder.literal_name_reference(wire, idx, base, &self.table)?
            } else if wire[idx] & FieldType::LITERAL_LITERAL_NAME == FieldType::LITERAL_LITERAL_NAME {
                self.decoder.literal_literal_name(wire, idx)?
            } else if wire[idx] & FieldType::INDEXED_POST_BASE_INDEX == FieldType::INDEXED_POST_BASE_INDEX {
                self.decoder.indexed_post_base_index(wire, idx, base, &self.table)?
            } else if wire[idx] & 0b11110000 == FieldType::LITERAL_POST_BASE_NAME_REFERENCE {
                self.decoder.literal_post_base_name_reference(wire, idx, base, &self.table)?
            } else {
                return Err(DecompressionFailed.into());
            };
            idx += ret.0;
            headers.push(ret.1);
        }
        self.decoder.pending_sections.insert(stream_id, requred_insert_count as usize);
        Ok(headers)
    }
    pub fn decode_encoder_instruction(&mut self, wire: &Vec<u8>) -> Result<(), Box<dyn error::Error>> {
        let mut idx = 0;
        let wire_len = wire.len();
        while idx < wire_len {
            idx += if wire[idx] & encoder::Instruction::INSERT_WITH_NAME_REFERENCE == encoder::Instruction::INSERT_WITH_NAME_REFERENCE {
                self.decoder.insert_with_name_reference(wire, idx, &mut self.table)?
            } else if wire[idx] & encoder::Instruction::INSERT_WITH_LITERAL_NAME == encoder::Instruction::INSERT_WITH_LITERAL_NAME {
                self.decoder.insert_with_literal_name(wire, idx, &mut self.table)?
            } else if wire[idx] & encoder::Instruction::SET_DYNAMIC_TABLE_CAPACITY == encoder::Instruction::SET_DYNAMIC_TABLE_CAPACITY {
                self.decoder.dynamic_table_capacity(wire, idx, &mut self.table)?
            } else { // if wire[idx] & encoder::Instruction::DUPLICATE == encoder::Instruction::DUPLICATE
                self.decoder.duplicate(wire, idx, &mut self.table)?
            };
        }
        Ok(())
    }
    pub fn decode_decoder_instruction(&mut self, wire: &Vec<u8>) -> Result<(), Box<dyn error::Error>> {
        let mut idx = 0;
        let wire_len = wire.len();
        while idx < wire_len {
            idx += if wire[idx] & decoder::Instruction::SECTION_ACKNOWLEDGMENT == decoder::Instruction::SECTION_ACKNOWLEDGMENT {
                let (len, stream_id) = self.encoder.section_ackowledgment(wire, idx)?;

                let section = self.encoder.ack_section(stream_id);
                self.table.ack_section(section);
                len
            } else if wire[idx] & decoder::Instruction::STREAM_CANCELLATION == decoder::Instruction::STREAM_CANCELLATION {
                let (len, _stream_id) = self.encoder.stream_cancellation(wire, idx)?;
                // TODO: manage stream_id
                len
            } else { // wire[idx] & Instruction::INSERT_COUNT_INCREMENT == Instruction::INSERT_COUNT_INCREMENT
                let (len, increment) = self.encoder.insert_count_increment(wire, idx)?;
                if increment == 0 || self.encoder.known_sending_count < (*self.table.dynamic_read).borrow().acked_section + increment {
                    return Err(DecoderStreamError.into());
                }
                (*self.table.dynamic_table).borrow_mut().acked_section += increment;
                len
            };
        }
        Ok(())
    }
    pub fn dump_dynamic_table(&self) {
        self.table.dump_dynamic_table();
    }
}

struct Qnum;
impl Qnum {
    pub fn encode(encoded: &mut Vec<u8>, val: u32, n: u8) -> usize {
		let mut val = val;
        let mut len = 1;
        if val < (1 << n) - 1 {
            encoded.push(val as u8);
            return len;
        }
        encoded.push((1 << n) - 1);
        val -= (1 << n) - 1;
        while val >= 128 {
            encoded.push(((val & 0b01111111) | 0b10000000) as u8);
            val = val >> 7;
            len += 1;
        }
        encoded.push(val as u8);
        return len + 1;
    }
    pub fn decode(encoded: &Vec<u8>, idx: usize, n: u8) -> (usize, u32) {
        let mask: u16 = (1 << n) - 1;
        let prefix = encoded[idx] & mask as u8;
        if prefix < mask as u8 {
            return (1, prefix as u32);
        }
        let mut len = 1;
        let mut m = 0;
        let mut next = true;
        let mut val = 0;
        while next {
            val += ((encoded[idx + len] & 0b01111111) as u32) << m;
            next = encoded[idx + len] & 0b10000000 == 0b10000000;
            m += 7;
            len += 1;
        }
        (len, val + prefix as u32)
    }
}

struct FieldType;
impl FieldType {
    // 4.5.2
    // 0   1   2   3   4   5   6   7
    // +---+---+---+---+---+---+---+---+
    // | 1 | T |      Index (6+)       |
    // +---+---+-----------------------+
    pub const INDEXED: u8 = 0b10000000;
    // 4.5.3
    // 0   1   2   3   4   5   6   7
    // +---+---+---+---+---+---+---+---+
    // | 0 | 0 | 0 | 1 |  Index (4+)   |
    // +---+---+---+---+---------------+
    pub const INDEXED_POST_BASE_INDEX: u8 = 0b00010000;
    // 4.5.4
    // 0   1   2   3   4   5   6   7
    // +---+---+---+---+---+---+---+---+
    // | 0 | 1 | N | T |Name Index (4+)|
    // +---+---+---+---+---------------+
    // | H |     Value Length (7+)     |
    // +---+---------------------------+
    // |  Value String (Length bytes)  |
    // +-------------------------------+
    pub const LITERAL_NAME_REFERENCE: u8 = 0b01000000;
    // 4.5.5
    // 0   1   2   3   4   5   6   7
    // +---+---+---+---+---+---+---+---+
    // | 0 | 0 | 0 | 0 | N |NameIdx(3+)|
    // +---+---+---+---+---+-----------+
    // | H |     Value Length (7+)     |
    // +---+---------------------------+
    // |  Value String (Length bytes)  |
    // +-------------------------------+
    pub const LITERAL_POST_BASE_NAME_REFERENCE: u8 = 0b00000000;
    // 4.5.6
    // 0   1   2   3   4   5   6   7
    // +---+---+---+---+---+---+---+---+
    // | 0 | 0 | 1 | N | H |NameLen(3+)|
    // +---+---+---+---+---+-----------+
    // |  Name String (Length bytes)   |
    // +---+---------------------------+
    // | H |     Value Length (7+)     |
    // +---+---------------------------+
    // |  Value String (Length bytes)  |
    // +-------------------------------+
    pub const LITERAL_LITERAL_NAME: u8 = 0b00100000;
}

#[derive(Debug)]
struct DecompressionFailed; // TODO: represent 0x0200
impl error::Error for DecompressionFailed {}
impl fmt::Display for DecompressionFailed {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "Decompression Failed")
	}
}
#[derive(Debug)]
struct EncoderStreamError; // TODO: represent 0x0201
impl error::Error for EncoderStreamError {}
impl fmt::Display for EncoderStreamError {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "Encoder Stream Error")
	}
}
#[derive(Debug)]
struct DecoderStreamError; // TODO: represent 0x0202
impl error::Error for DecoderStreamError {}
impl fmt::Display for DecoderStreamError {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "Decoder Stream Error")
	}
}
// StrHeader will be implemented later once all works
// I assume &str header's would be slow due to page fault
type StrHeader<'a> = (&'a str, &'a str);
#[derive(PartialEq, Eq, Debug)]
pub struct Header(String, String);

impl Header {
    pub fn from(name: &str, value: &str) -> Self {
        Self(String::from(name), String::from(value))
    }
    pub fn from_string(name: String, value: String) -> Self {
        Self(name, value)
    }
    pub fn from_str_header(header: StrHeader) -> Self {
        Self(header.0.to_string(), header.1.to_string())
    }
}

#[cfg(test)]
mod tests {
	use crate::{Header, Qpack};
    static STREAM_ID: u16 = 4;
	#[test]
	fn rfc_appendix_b1_encode() {
		let mut qpack = Qpack::new(1024);
		let headers = vec![Header::from(":path", "/index.html")];
        let mut encoded = vec![];
		let _ = qpack.encode_headers(&mut encoded, false, headers, STREAM_ID);
		assert_eq!(encoded,
					vec![0x00, 0x00, 0x51, 0x0b, 0x2f,
						 0x69, 0x6e, 0x64, 0x65, 0x78,
						 0x2e, 0x68, 0x74, 0x6d, 0x6c]);
	}
	#[test]
	fn rfc_appendix_b1_decode() {
		let mut qpack = Qpack::new(1024);
		let wire = vec![0x00, 0x00, 0x51, 0x0b, 0x2f,
								0x69, 0x6e, 0x64, 0x65, 0x78,
								0x2e, 0x68, 0x74, 0x6d, 0x6c];
		let out = qpack.decode_headers(&wire, STREAM_ID).unwrap();
		assert_eq!(out, vec![Header::from(":path", "/index.html")]);
	}

	#[test]
	fn encode_indexed_simple() {
		let mut qpack = Qpack::new(1024);
		let headers = vec![Header::from(":path", "/")];
        let mut encoded = vec![];
		let _ = qpack.encode_headers(&mut encoded, false, headers, STREAM_ID);
		assert_eq!(encoded,
			vec![0x00, 0x00, 0xc1]);
	}
	#[test]
	fn decode_indexed_simple() {
		let mut qpack = Qpack::new(1024);
		let wire = vec![0x00, 0x00, 0xc1];
		let out = qpack.decode_headers(&wire, STREAM_ID).unwrap();
		assert_eq!(out,
			vec![Header::from(":path", "/")]);
	}
    #[test]
    fn encode_set_dynamic_table_capacity() {
        let mut qpack = Qpack::new(1024);
        let mut encoded = vec![];
        let _ = qpack.encode_set_dynamic_table_capacity(&mut encoded, 220);
        assert_eq!(encoded, vec![0x3f, 0xbd, 0x01]);
    }
    #[test]
    fn encode_insert_with_name_reference() {
		let mut qpack_encoder = Qpack::new(1024);
        let mut qpack_decoder = Qpack::new(1024);

        println!("Step 1");
        {   // encoder instruction
            let mut encoded = vec![];
            let _ = qpack_encoder.encode_set_dynamic_table_capacity(&mut encoded, 220);
            let _ = qpack_encoder.encode_insert_header(&mut encoded,Header::from(":authority", "www.example.com"));
            let _ = qpack_encoder.encode_insert_header(&mut encoded,Header::from(":path", "/sample/path"));

            let _ = qpack_decoder.decode_encoder_instruction(&encoded);

            println!("dump encoder side dynamic table");
            qpack_encoder.dump_dynamic_table();
            println!("dump decoder side dynamic table");
            qpack_decoder.dump_dynamic_table();
        }

        println!("Step 2");
        {   // header transfer
            let mut encoded = vec![];
            let _ = qpack_encoder.encode_headers(&mut encoded, true,
                vec![Header::from(":authority", "www.example.com"),
                            Header::from(":path", "/sample/path")], STREAM_ID);
            println!("{:02x?}", &encoded);
            if let Ok(headers) = qpack_decoder.decode_headers(&encoded, STREAM_ID) {
                println!("{:?}", headers);
                // assert
            }
        }

        println!("Step 3");
        {   // decoder instruction
            let mut encoded = vec![];
            let _ = qpack_decoder.encode_section_ackowledgment(&mut encoded, STREAM_ID);
            println!("{:02x?}", encoded);

            let _ = qpack_encoder.decode_decoder_instruction(&encoded);
            println!("dump encoder side dynamic table");
            qpack_encoder.dump_dynamic_table();
            println!("dump decoder side dynamic table");
            qpack_decoder.dump_dynamic_table();
        }

        println!("Step 4");
        {   // encoder instruction
            let mut encoded = vec![];
            let _ = qpack_encoder.encode_insert_header(&mut encoded, Header::from("custom-key", "custom-value"));
            println!("Step 4 wire: {:02x?}", &encoded);

            let _ = qpack_decoder.decode_encoder_instruction(&encoded);
            println!("dump encoder side dynamic table");
            qpack_encoder.dump_dynamic_table();
            println!("dump decoder side dynamic table");
            qpack_decoder.dump_dynamic_table();
        }

        println!("Step 5");
        {   // decoder instruction
            let mut encoded = vec![];
            let _ = qpack_decoder.encode_insert_count_increment(&mut encoded);
            println!("{:02x?}", encoded);

            let _ = qpack_encoder.decode_decoder_instruction(&encoded);

            qpack_encoder.dump_dynamic_table();
            qpack_decoder.dump_dynamic_table();
        }

        println!("Step 6");
        {   // encoder instruction
            let mut encoded = vec![];
            let _ = qpack_encoder.encode_insert_header(&mut encoded, Header::from(":authority", "www.example.com"));
            println!("{:02x?}", encoded);
            let _= qpack_decoder.decode_encoder_instruction(&encoded);

            qpack_encoder.dump_dynamic_table();
            qpack_decoder.dump_dynamic_table();
        }

        println!("Step 7");
        {   // header transfer
            let mut encoded = vec![];
            let _ = qpack_encoder.encode_headers(&mut encoded, false,
                        vec![Header::from(":authority", "www.example.com"),
                                    Header::from(":path", "/"),
                                    Header::from("custom-key", "custom-value")], 8);
            println!("{:02x?}", encoded);

            if let Ok(headers) = qpack_decoder.decode_headers(&encoded, 8) {
                println!("{:?}", headers);
            }
        }

        println!("Step 8");
        {   // stream cancellation
            let mut encoded = vec![];
            let _ = qpack_decoder.encode_stream_cancellation(&mut encoded, 8);
            println!("{:02x?}", encoded);

            let _ = qpack_encoder.decode_decoder_instruction(&encoded);
        }

        println!("Step 9");
        {   // encoder instruction
            let mut encoded = vec![];
            let _ = qpack_encoder.encode_insert_header(&mut encoded,Header::from("custom-key", "custom-value2"));
            println!("{:02x?}", encoded);

            let _ = qpack_decoder.decode_encoder_instruction(&encoded);

            println!("dump encoder side dynamic table");
            qpack_encoder.dump_dynamic_table();
            println!("dump decoder side dynamic table");
            qpack_decoder.dump_dynamic_table();
        }
    }
}