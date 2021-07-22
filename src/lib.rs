use crate::decoder::Decoder;
use crate::encoder::Encoder;
use crate::table::Table;
use core::fmt;
use std::error;
use std::sync::{Arc, RwLock};

mod decoder;
mod dynamic_table;
mod encoder;
mod table;

pub struct Qpack {
    encoder: Encoder,
    decoder: Decoder,
    table: Arc<RwLock<Table>>,
}

impl Qpack {
    pub fn new(dynamic_table_max_capacity: usize) -> Self {
        Qpack {
            encoder: Encoder::new(),
            decoder: Decoder::new(),
            table: Arc::new(RwLock::new(Table::new(dynamic_table_max_capacity))),
        }
    }
    pub fn encode_insert_headers(&self, encoded: &mut Vec<u8>, headers: Vec<Header>) -> Result<(), Box<dyn error::Error>> {
        for header in headers {
            let (both_match, on_static, idx) = self.table.read().unwrap().find_index(&header);
            let idx = if idx != (1 << 32) - 1 && !on_static {
                // absolute to relative conversion
                self.table.read().unwrap().get_insert_count() - 1 - idx
            } else { idx };

            if both_match && !on_static {
                self.encoder.duplicate(encoded, idx)?;
            } else if idx != (1 << 32) - 1 {
                self.encoder.insert_with_name_reference(encoded, on_static, idx, header.1)?;
            } else {
                self.encoder.insert_with_literal_name(encoded, header.0, header.1)?;
            }
        }
        Ok(())
    }
    // TODO: can be optimized
    pub fn insert_headers(&mut self, headers: Vec<Header>) -> Result<(), Box<dyn error::Error>> {
        let count =  headers.len();
        for header in headers {
            self.table.write().unwrap().insert(header)?;
        }
        self.encoder.known_sending_count += count;
        Ok(())
    }
    pub fn encode_set_dynamic_table_capacity(&self, encoded: &mut Vec<u8>, capacity: usize) -> Result<(), Box<dyn error::Error>> {
        self.encoder.set_dynamic_table_capacity(encoded, capacity)
    }
    pub fn commit_set_dynamic_table_capacity(&mut self, capacity: usize) -> Result<(), Box<dyn error::Error>> {
        self.table.write().unwrap().set_dynamic_table_capacity(capacity)
    }
    pub fn encode_section_ackowledgment(&self, encoded: &mut Vec<u8>, stream_id: u16) -> Result<(), Box<dyn error::Error>> {
        self.decoder.section_ackowledgment(encoded, stream_id)
    }
    pub fn commit_section_ackowledgment(&mut self, stream_id: u16) -> Result<(), Box<dyn error::Error>> {
        let section = self.decoder.ack_section(stream_id);
        self.table.write().unwrap().dynamic_table.ack_section(section);
        Ok(())
    }
    pub fn encode_stream_cancellation(&self, encoded: &mut Vec<u8>, stream_id: u16) -> Result<(), Box<dyn error::Error>> {
        self.decoder.stream_cancellation(encoded, stream_id)
    }
    pub fn commit_stream_cancellation(&mut self, stream_id: u16) -> Result<(), Box<dyn error::Error>> {
        self.decoder.cancel_section(stream_id);
        Ok(())
    }
    // TODO: check whether to update state
    pub fn encode_insert_count_increment(&self, encoded: &mut Vec<u8>) -> Result<usize, Box<dyn error::Error>> {
        let increment = self.table.read().unwrap().dynamic_table.list.len() - self.table.read().unwrap().dynamic_table.known_received_count;
        self.decoder.insert_count_increment(encoded, increment)?;
        Ok(increment)
    }
    pub fn commit_insert_count_increment(&mut self, increment: usize) -> Result<(), Box<dyn error::Error>> {
        self.table.write().unwrap().dynamic_table.known_received_count += increment;
        Ok(())
    }
    pub fn encode_headers(&self, encoded: &mut Vec<u8>, relative_indexing: bool, headers: Vec<Header>) -> Result<usize, Box<dyn error::Error>> {
        // TODO: decide whether to use s_flag (relative indexing)
        let required_insert_count = self.table.read().unwrap().get_insert_count();
        // TODO: suspicious
        let base = if relative_indexing {
            0
        } else {
            self.table.read().unwrap().dynamic_table.insert_count as u32
        };
        self.encoder.prefix(encoded, &self.table.read().unwrap(), required_insert_count as u32, relative_indexing, base);

        for header in headers {
            let (both_match, on_static, idx) = self.table.read().unwrap().find_index(&header);
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
                } else {
                    self.encoder.literal_name_reference(encoded, idx as u32, &header.1, on_static);
                }
            } else { // not found
                self.encoder.literal_literal_name(encoded, &header);
            }
        }
        Ok(required_insert_count)
    }
    pub fn commit_sent_headers(&mut self, stream_id: u16, required_insert_count: usize) -> Result<(), Box<dyn error::Error>> {
        self.encoder.pending_sections.insert(stream_id, required_insert_count);
        Ok(())
    }

    pub fn decode_headers(&mut self, wire: &Vec<u8>, stream_id: u16) -> Result<(Vec<Header>, bool), Box<dyn error::Error>> {
        let mut idx = 0;
        let (len, requred_insert_count, base) = self.decoder.prefix(wire, idx, &self.table.read().unwrap())?;
        idx += len;

        // blocked if dynamic_table.insert_count < requred_insert_count
        // blocked just before referencing dynamic_table is better?

        let mut headers = vec![];
        let wire_len = wire.len();
        let mut ref_dynamic = false;
        while idx < wire_len {
            let ret = if wire[idx] & FieldType::INDEXED == FieldType::INDEXED {
                self.decoder.indexed(wire, &mut idx, base, &self.table.read().unwrap())?
            } else if wire[idx] & FieldType::LITERAL_NAME_REFERENCE == FieldType::LITERAL_NAME_REFERENCE {
                self.decoder.literal_name_reference(wire, &mut idx, base, &self.table.read().unwrap())?
            } else if wire[idx] & FieldType::LITERAL_LITERAL_NAME == FieldType::LITERAL_LITERAL_NAME {
                self.decoder.literal_literal_name(wire, &mut idx)?
            } else if wire[idx] & FieldType::INDEXED_POST_BASE_INDEX == FieldType::INDEXED_POST_BASE_INDEX {
                self.decoder.indexed_post_base_index(wire, &mut idx, base, &self.table.read().unwrap())?
            } else if wire[idx] & 0b11110000 == FieldType::LITERAL_POST_BASE_NAME_REFERENCE {
                self.decoder.literal_post_base_name_reference(wire, &mut idx, base, &self.table.read().unwrap())?
            } else {
                return Err(DecompressionFailed.into());
            };
            headers.push(ret.0);
            ref_dynamic |= ret.1;
        }
        self.decoder.pending_sections.insert(stream_id, requred_insert_count as usize);
        Ok((headers, ref_dynamic))
    }
    pub fn decode_encoder_instruction(&self, wire: &Vec<u8>) -> Result<Box<dyn FnOnce() -> Result<(), Box<dyn error::Error>>>,
                                                                        Box<dyn error::Error>> {
        let mut idx = 0;
        let wire_len = wire.len();
        let mut commit_funcs: Vec<Box<dyn FnOnce() -> Result<(), Box<dyn error::Error>>>> = vec![];

        while idx < wire_len {
            let table_c = Arc::clone(&self.table);
            idx += if wire[idx] & encoder::Instruction::INSERT_WITH_NAME_REFERENCE == encoder::Instruction::INSERT_WITH_NAME_REFERENCE {
                let (output, input) = self.decoder.insert_with_name_reference(wire, idx)?;
                commit_funcs.push(Box::new(move || -> Result<(), Box<dyn error::Error>> {
                    table_c.write().unwrap().insert_with_name_reference(input.0, input.1, input.2)
                }));
                output
            } else if wire[idx] & encoder::Instruction::INSERT_WITH_LITERAL_NAME == encoder::Instruction::INSERT_WITH_LITERAL_NAME {
                let (output, input) = self.decoder.insert_with_literal_name(wire, idx)?;
                commit_funcs.push(Box::new(move || -> Result<(), Box<dyn error::Error>> {
                    table_c.write().unwrap().insert_with_literal_name(input.0, input.1)
                }));
                output
            } else if wire[idx] & encoder::Instruction::SET_DYNAMIC_TABLE_CAPACITY == encoder::Instruction::SET_DYNAMIC_TABLE_CAPACITY {
                let (output, input) = self.decoder.dynamic_table_capacity(wire, idx)?;
                commit_funcs.push(Box::new(move || -> Result<(), Box<dyn error::Error>> {
                    table_c.write().unwrap().set_dynamic_table_capacity(input)
                }));
                output
            } else { // if wire[idx] & encoder::Instruction::DUPLICATE == encoder::Instruction::DUPLICATE
                let (output, input) = self.decoder.duplicate(wire, idx)?;
                commit_funcs.push(Box::new(move || -> Result<(), Box<dyn error::Error>> {
                    table_c.write().unwrap().duplicate(input)
                }));
                output
            };
        }
        Ok(Box::new(move || -> Result<(), Box<dyn error::Error>> {
            for f in commit_funcs {
                f()?;
            }
            Ok(())
        }))
    }

    pub fn decode_decoder_instruction(&mut self, wire: &Vec<u8>) -> Result<(), Box<dyn error::Error>> {
        let mut idx = 0;
        let wire_len = wire.len();
        while idx < wire_len {
            idx += if wire[idx] & decoder::Instruction::SECTION_ACKNOWLEDGMENT == decoder::Instruction::SECTION_ACKNOWLEDGMENT {
                let (len, _stream_id) = self.encoder.section_ackowledgment(wire, idx, &mut self.table.write().unwrap())?;
                len
            } else if wire[idx] & decoder::Instruction::STREAM_CANCELLATION == decoder::Instruction::STREAM_CANCELLATION {
                let (len, _stream_id) = self.encoder.stream_cancellation(wire, idx)?;
                len
            } else { // wire[idx] & Instruction::INSERT_COUNT_INCREMENT == Instruction::INSERT_COUNT_INCREMENT
                let (len, increment) = self.encoder.insert_count_increment(wire, idx)?;
                if increment == 0 || self.encoder.known_sending_count < self.table.read().unwrap().dynamic_table.known_received_count + increment {
                    return Err(DecoderStreamError.into());
                }
                self.table.write().unwrap().dynamic_table.known_received_count += increment;
                len
            };
        }
        Ok(())
    }
    pub fn dump_dynamic_table(&self) {
        self.table.read().unwrap().dump_dynamic_table();
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
        let mut val: u32 = (encoded[idx] & mask as u8) as u32;
        let mut next = val as u16 == mask;

        let mut len = 1;
        let mut m = 0;
        while next {
            val += ((encoded[idx + len] & 0b01111111) as u32) << m;
            next = encoded[idx + len] & 0b10000000 == 0b10000000;
            m += 7;
            len += 1;
        }
        (len, val)
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
#[derive(PartialEq, Eq, Debug, Clone)]
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
    use std::{error, sync::{Arc, RwLock}, thread};
    use crate::{Header, Qpack};
    static STREAM_ID: u16 = 4;
	#[test]
	fn rfc_appendix_b1_encode() {
		let qpack = Qpack::new(1024);
		let headers = vec![Header::from(":path", "/index.html")];
        let mut encoded = vec![];
		qpack.encode_headers(&mut encoded, false, headers).unwrap();
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
		assert_eq!(out.0, vec![Header::from(":path", "/index.html")]);
		assert_eq!(out.1, false);
	}

	#[test]
	fn encode_indexed_simple() {
		let qpack = Qpack::new(1024);
		let headers = vec![Header::from(":path", "/")];
        let mut encoded = vec![];
		let _ = qpack.encode_headers(&mut encoded, false, headers);
		assert_eq!(encoded,
			vec![0x00, 0x00, 0xc1]);
	}
	#[test]
	fn decode_indexed_simple() {
		let mut qpack = Qpack::new(1024);
		let wire = vec![0x00, 0x00, 0xc1];
		let out = qpack.decode_headers(&wire, STREAM_ID).unwrap();
		assert_eq!(out.0,
			vec![Header::from(":path", "/")]);
        assert_eq!(out.1, false);
	}
    #[test]
    fn encode_set_dynamic_table_capacity() {
        let qpack = Qpack::new(1024);
        let mut encoded = vec![];
        let _ = qpack.encode_set_dynamic_table_capacity(&mut encoded, 220);
        assert_eq!(encoded, vec![0x3f, 0xbd, 0x01]);
    }
    #[test]
    fn multi_threading() {
        let qpack_encoder = Qpack::new(1024);
        let qpack_decoder = Qpack::new(1024);
        let safe_encoder = Arc::new(RwLock::new(qpack_encoder));
        let safe_decoder = Arc::new(RwLock::new(qpack_decoder));

        let f = |headers: Vec<Header>, stream_id: u16, expected_wire: Vec<u8>,
                                                encoder: Arc<RwLock<Qpack>>, decoder: Arc<RwLock<Qpack>>| -> Result<(), Box<dyn error::Error>> {
            let mut encoded = vec![];
            let required_insert_count = encoder.read().unwrap().encode_headers(&mut encoded, false, headers.clone())?;
            let _ = encoder.write().unwrap().commit_sent_headers(stream_id, required_insert_count);
            //assert_eq!(encoded, expected_wire);

            if let Ok(out) = decoder.write().unwrap().decode_headers(&encoded, stream_id) {
                assert_eq!(out.0, headers);
                assert_eq!(out.1, false);
            } else {
                assert!(false);
            }
            Ok(())
        };

        let mut ths = vec![];
        let headers_set = vec![vec![Header::from(":path", "/"), Header::from("age", "0")],
                                            vec![Header::from("content-length", "0"), Header::from(":method", "CONNECT")]];
        let expected_wires: Vec<Vec<u8>> = vec![vec![], vec![]];
        for i in 0..headers_set.len() {
            let en = Arc::clone(&safe_encoder);
            let de = Arc::clone(&safe_decoder);
            let headers = headers_set[i].clone();
            let expected_wire = expected_wires[i].clone();
            ths.push(thread::spawn(move || {
                let _ = f(headers, 4 + (i as u16) * 2,
                        expected_wire, en , de);
            }));
        }
        for th in ths {
            let _ = th.join();
        }
    }
    #[test]
    fn encode_insert_with_name_reference() {
        let mut qpack_encoder = Qpack::new(1024);
        let mut qpack_decoder = Qpack::new(1024);

        println!("Step 1");
        {   // encoder instruction
            let mut encoded = vec![];
            let capacity = 220;
            let _ = qpack_encoder.encode_set_dynamic_table_capacity(&mut encoded, capacity);
            let _ = qpack_encoder.commit_set_dynamic_table_capacity(capacity);
            let headers = vec![Header::from(":authority", "www.example.com"),
                                          Header::from(":path", "/sample/path")];

            let _ = qpack_encoder.encode_insert_headers(&mut encoded, headers.clone());
            assert_eq!(encoded, vec![0x3f, 0xbd, 0x01, 0xc0, 0x0f, 0x77, 0x77,
                                    0x77, 0x2e, 0x65, 0x78, 0x61, 0x6d, 0x70,
                                    0x6c, 0x65, 0x2e, 0x63, 0x6f, 0x6d, 0xc1,
                                    0x0c, 0x2f, 0x73, 0x61, 0x6d, 0x70, 0x6c,
                                    0x65, 0x2f, 0x70, 0x61, 0x74, 0x68]);
            let _ = qpack_encoder.insert_headers(headers);

            let commit_func = qpack_decoder.decode_encoder_instruction(&encoded);
            let _ = commit_func.unwrap()();

            qpack_encoder.dump_dynamic_table();
            qpack_decoder.dump_dynamic_table();
        }

        println!("Step 2");
        {   // header transfer
            let mut encoded = vec![];
            let headers = vec![Header::from(":authority", "www.example.com"),
                                          Header::from(":path", "/sample/path")];
            let required_insert_count = qpack_encoder.encode_headers(&mut encoded, true, headers.clone()).unwrap();
            let _ = qpack_encoder.commit_sent_headers(STREAM_ID, required_insert_count);
            assert_eq!(encoded, vec![0x03, 0x81, 0x10, 0x11]);

            if let Ok(decoded) = qpack_decoder.decode_headers(&encoded, STREAM_ID) {
                assert_eq!(decoded.0, headers);
                assert_eq!(decoded.1, true);
            } else {
                assert!(false);
            }
        }

        println!("Step 3");
        {   // decoder instruction
            let mut encoded = vec![];
            let _ = qpack_decoder.encode_section_ackowledgment(&mut encoded, STREAM_ID);
            assert_eq!(encoded, vec![0x84]);
            let _= qpack_decoder.commit_section_ackowledgment(STREAM_ID);

            let _ = qpack_encoder.decode_decoder_instruction(&encoded);
            println!("dump encoder side dynamic table");
            qpack_encoder.dump_dynamic_table();
            println!("dump decoder side dynamic table");
            qpack_decoder.dump_dynamic_table();
        }

        println!("Step 4");
        {   // encoder instruction
            let mut encoded = vec![];
            let headers = vec![Header::from("custom-key", "custom-value")];
            let _ = qpack_encoder.encode_insert_headers(&mut encoded, headers.clone());
            assert_eq!(encoded, vec![0x4a, 0x63, 0x75, 0x73, 0x74, 0x6f, 0x6d, 0x2d, 0x6b, 0x65,
                                    0x79, 0x0c, 0x63, 0x75, 0x73, 0x74, 0x6f, 0x6d, 0x2d, 0x76,
                                    0x61, 0x6c, 0x75, 0x65]);
            let _ = qpack_encoder.insert_headers(headers);

            let commit_func = qpack_decoder.decode_encoder_instruction(&encoded);
            let _ = commit_func.unwrap()();
            println!("dump encoder side dynamic table");
            qpack_encoder.dump_dynamic_table();
            println!("dump decoder side dynamic table");
            qpack_decoder.dump_dynamic_table();
        }

        println!("Step 5");
        {   // decoder instruction
            let mut encoded = vec![];
            let increment = qpack_decoder.encode_insert_count_increment(&mut encoded).unwrap();
            assert_eq!(encoded, vec![0x01]);
            let _ = qpack_decoder.commit_insert_count_increment(increment);

            let _ = qpack_encoder.decode_decoder_instruction(&encoded);

            qpack_encoder.dump_dynamic_table();
            qpack_decoder.dump_dynamic_table();
        }

        println!("Step 6");
        {   // encoder instruction
            let mut encoded = vec![];
            let headers = vec![Header::from(":authority", "www.example.com")];
            let _ = qpack_encoder.encode_insert_headers(&mut encoded, headers.clone());
            assert_eq!(encoded, vec![0x02]);
            let _ = qpack_encoder.insert_headers(headers);

            let commit_func = qpack_decoder.decode_encoder_instruction(&encoded);
            let _ = commit_func.unwrap()();

            qpack_encoder.dump_dynamic_table();
            qpack_decoder.dump_dynamic_table();
        }

        println!("Step 7");
        {   // header transfer
            let mut encoded = vec![];
            let headers = vec![Header::from(":authority", "www.example.com"),
                                        Header::from(":path", "/"),
                                        Header::from("custom-key", "custom-value")];
            let required_insert_count = qpack_encoder.encode_headers(&mut encoded, false,
                        headers.clone()).unwrap();
            let _ = qpack_encoder.commit_sent_headers(8, required_insert_count);
            assert_eq!(encoded, vec![0x05, 0x00, 0x80, 0xc1, 0x81]);

            if let Ok(decoded) = qpack_decoder.decode_headers(&encoded, 8) {
                assert_eq!(decoded.0, headers);
                assert_eq!(decoded.1, true);
            } else {
                assert!(false);
            }
        }

        println!("Step 8");
        {   // stream cancellation
            let mut encoded = vec![];
            let _ = qpack_decoder.encode_stream_cancellation(&mut encoded, 8);
            assert_eq!(encoded, vec![0x48]);
            let _= qpack_decoder.commit_stream_cancellation(8);

            let _ = qpack_encoder.decode_decoder_instruction(&encoded);
        }

        println!("Step 9");
        {   // encoder instruction
            let mut encoded = vec![];
            let headers = vec![Header::from("custom-key", "custom-value2")];
            let _ = qpack_encoder.encode_insert_headers(&mut encoded, headers.clone());
            assert_eq!(encoded, vec![0x81, 0x0d, 0x63, 0x75, 0x73, 0x74, 0x6f, 0x6d,
                                     0x2d, 0x76, 0x61, 0x6c, 0x75, 0x65, 0x32]);
            let _ = qpack_encoder.insert_headers(headers);

            let commit_func = qpack_decoder.decode_encoder_instruction(&encoded);
            let _ = commit_func.unwrap()();

            println!("dump encoder side dynamic table");
            qpack_encoder.dump_dynamic_table();
            println!("dump decoder side dynamic table");
            qpack_decoder.dump_dynamic_table();
        }
    }
}