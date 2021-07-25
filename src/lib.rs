use crate::decoder::Decoder;
use crate::encoder::Encoder;
use crate::table::Table;
use core::fmt;
use std::error;
use std::sync::{Arc, Condvar, Mutex, RwLock};

mod decoder;
mod dynamic_table;
mod encoder;
mod table;

pub struct Qpack {
    encoder: Arc<RwLock<Encoder>>,
    decoder: Arc<RwLock<Decoder>>,
    table: Table,
    blocked_streams_limit: u16,
    cv: Arc<(Mutex<usize>, Condvar)>,
}

impl Qpack {
    pub fn new(blocked_streams_limit: u16, dynamic_table_max_capacity: usize) -> Self {
        let cv = Arc::new((Mutex::new(0), Condvar::new()));
        Qpack {
            encoder: Arc::new(RwLock::new(Encoder::new())),
            decoder: Arc::new(RwLock::new(Decoder::new())),
            table: Table::new(dynamic_table_max_capacity, Arc::clone(&cv)),
            blocked_streams_limit,
            cv,
        }
    }
    pub fn encode_insert_headers(&self, encoded: &mut Vec<u8>, headers: Vec<Header>)
        -> Result<Box<dyn FnOnce() -> Result<(), Box<dyn error::Error>>>,
                    Box<dyn error::Error>> {
        for header in &headers {
            let (both_match, on_static, idx) = self.table.find_index(header);
            let idx = if idx != (1 << 32) - 1 && !on_static {
                // absolute to relative conversion
                self.table.get_insert_count() - 1 - idx
            } else { idx };

            if both_match && !on_static {
                Encoder::duplicate(encoded, idx)?;
            } else if idx != (1 << 32) - 1 {
                Encoder::insert_with_name_reference(encoded, on_static, idx, &header.1)?;
            } else {
                Encoder::insert_with_literal_name(encoded, &header.0, &header.1)?;
            }
        }
        let dynamic_table = Arc::clone(&self.table.dynamic_table);
        let known_sending_count = Arc::clone(&self.encoder.read().unwrap().known_sending_count);
        Ok(Box::new(move || -> Result<(), Box<dyn error::Error>> {
            let mut locked = dynamic_table.write().unwrap();
            let count = headers.len();
            for header in headers {
                locked.insert(header)?;
            }
            (*known_sending_count.write().unwrap()) += count;
            Ok(())
        }))
    }
    pub fn encode_set_dynamic_table_capacity(&self, encoded: &mut Vec<u8>, capacity: usize)
        -> Result<Box<dyn FnOnce() -> Result<(), Box<dyn error::Error>>>,
                  Box<dyn error::Error>> {
        Encoder::set_dynamic_table_capacity(encoded, capacity)?;
        let dynamic_table = Arc::clone(&self.table.dynamic_table);
        Ok(Box::new(move || -> Result<(), Box<dyn error::Error>> {
            dynamic_table.write().unwrap().set_capacity(capacity)
        }))
    }
    pub fn encode_section_ackowledgment(&self, encoded: &mut Vec<u8>, stream_id: u16)
        -> Result<Box<dyn FnOnce() -> Result<(), Box<dyn error::Error>>>,
                    Box<dyn error::Error>> {
        Decoder::section_ackowledgment(encoded, stream_id)?;

        let decoder = Arc::clone(&self.decoder);
        let dynamic_table = Arc::clone(&self.table.dynamic_table);
        Ok(Box::new(move || -> Result<(), Box<dyn error::Error>> {
            let section = decoder.write().unwrap().ack_section(stream_id);
            dynamic_table.write().unwrap().ack_section(section);
            Ok(())
        }))
    }
    pub fn encode_stream_cancellation(&self, encoded: &mut Vec<u8>, stream_id: u16)
        -> Result<Box<dyn FnOnce() -> Result<(), Box<dyn error::Error>>>,
                    Box<dyn error::Error>> {
        Decoder::stream_cancellation(encoded, stream_id)?;
        let decoder = Arc::clone(&self.decoder);
        Ok(Box::new(move || -> Result<(), Box<dyn error::Error>> {
            decoder.write().unwrap().cancel_section(stream_id);
            Ok(())
        }))
    }
    // TODO: check whether to update state
    pub fn encode_insert_count_increment(&self, encoded: &mut Vec<u8>)
        -> Result<Box<dyn FnOnce() -> Result<(), Box<dyn error::Error>>>,
                    Box<dyn error::Error>> {
        let dynamic_table_read = self.table.dynamic_table.read().unwrap();
        let increment = dynamic_table_read.list.len() - dynamic_table_read.known_received_count;
        Decoder::insert_count_increment(encoded, increment)?;
        let dynamic_table = Arc::clone(&self.table.dynamic_table);
        Ok(Box::new(move || -> Result<(), Box<dyn error::Error>> {
            dynamic_table.write().unwrap().known_received_count += increment;
            Ok(())
        }))
    }
    pub fn encode_headers(&self, encoded: &mut Vec<u8>, relative_indexing: bool, headers: Vec<Header>, stream_id: u16)
        -> Result<Box<dyn FnOnce() -> Result<(), Box<dyn error::Error>>>,
                    Box<dyn error::Error>> {
        // TODO: decide whether to use s_flag (relative indexing)
        let required_insert_count = self.table.get_insert_count();
        // TODO: suspicious
        let base = if relative_indexing {
            0
        } else {
            self.table.get_insert_count() as u32
        };
        Encoder::prefix(encoded, &self.table, required_insert_count as u32, relative_indexing, base);

        for header in headers {
            let (both_match, on_static, idx) = self.table.find_index(&header);
            if both_match {
                if relative_indexing {
                    Encoder::indexed_post_base_index(encoded, idx as u32);
                } else {
                    let abs_idx = if on_static { idx } else { base as usize - idx - 1 };
                    Encoder::indexed(encoded, abs_idx as u32, on_static);
                }
            } else if idx != (1 << 32) - 1 {
                if relative_indexing {
                    Encoder::literal_post_base_name_reference(encoded, idx as u32, &header.1);
                } else {
                    Encoder::literal_name_reference(encoded, idx as u32, &header.1, on_static);
                }
            } else { // not found
                Encoder::literal_literal_name(encoded, &header);
            }
        }
        let encoder = Arc::clone(&self.encoder);
        Ok(Box::new(move || -> Result<(), Box<dyn error::Error>> {
            if required_insert_count != 0 {
                encoder.write().unwrap().add_section(stream_id, required_insert_count);
            }
            Ok(())
        }))
    }

    pub fn decode_headers(&self, wire: &Vec<u8>, stream_id: u16) -> Result<(Vec<Header>, bool), Box<dyn error::Error>> {
        let mut idx = 0;
        let (len, requred_insert_count, base) = Decoder::prefix(wire, idx, &self.table)?;
        idx += len;

        // blocked if dynamic_table.insert_count < requred_insert_count
        // OPTIMIZE: blocked just before referencing dynamic_table is better?
        let insert_count = self.table.get_insert_count();
        if insert_count < requred_insert_count as usize {
            if self.blocked_streams_limit == self.decoder.read().unwrap().current_blocked_streams + 1 {
                return Err(DecompressionFailed.into());
            }
            self.decoder.write().unwrap().current_blocked_streams += 1;

            let (mux, cv) = &*self.cv;
            let mut locked_insert_count = mux.lock().unwrap();
            while *locked_insert_count < requred_insert_count as usize {
                locked_insert_count = cv.wait(locked_insert_count).unwrap();
            }
        }

        let mut headers = vec![];
        let wire_len = wire.len();
        let mut ref_dynamic = false;
        while idx < wire_len {
            let ret = if wire[idx] & FieldType::INDEXED == FieldType::INDEXED {
                Decoder::indexed(wire, &mut idx, base, &self.table)?
            } else if wire[idx] & FieldType::LITERAL_NAME_REFERENCE == FieldType::LITERAL_NAME_REFERENCE {
                Decoder::literal_name_reference(wire, &mut idx, base, &self.table)?
            } else if wire[idx] & FieldType::LITERAL_LITERAL_NAME == FieldType::LITERAL_LITERAL_NAME {
                Decoder::literal_literal_name(wire, &mut idx)?
            } else if wire[idx] & FieldType::INDEXED_POST_BASE_INDEX == FieldType::INDEXED_POST_BASE_INDEX {
                Decoder::indexed_post_base_index(wire, &mut idx, base, &self.table)?
            } else if wire[idx] & 0b11110000 == FieldType::LITERAL_POST_BASE_NAME_REFERENCE {
                Decoder::literal_post_base_name_reference(wire, &mut idx, base, &self.table)?
            } else {
                return Err(DecompressionFailed.into());
            };
            headers.push(ret.0);
            ref_dynamic |= ret.1;
        }
        // ?
        if requred_insert_count != 0 {
            self.decoder.write().unwrap().add_section(stream_id, requred_insert_count as usize);
        }
        Ok((headers, ref_dynamic))
    }
    pub fn decode_encoder_instruction(&self, wire: &Vec<u8>)
        -> Result<Box<dyn FnOnce() -> Result<(), Box<dyn error::Error>>>,
                    Box<dyn error::Error>> {
        let mut idx = 0;
        let wire_len = wire.len();
        let mut commit_funcs: Vec<Box<dyn FnOnce() -> Result<(), Box<dyn error::Error>>>> = vec![];

        while idx < wire_len {
            idx += if wire[idx] & encoder::Instruction::INSERT_WITH_NAME_REFERENCE == encoder::Instruction::INSERT_WITH_NAME_REFERENCE {
                let (output, input) = Decoder::insert_with_name_reference(wire, idx)?;
                commit_funcs.push(self.table.insert_with_name_reference(input.0, input.1, input.2)?);
                output
            } else if wire[idx] & encoder::Instruction::INSERT_WITH_LITERAL_NAME == encoder::Instruction::INSERT_WITH_LITERAL_NAME {
                let (output, input) = Decoder::insert_with_literal_name(wire, idx)?;
                commit_funcs.push(self.table.insert_with_literal_name(input.0, input.1)?);
                output
            } else if wire[idx] & encoder::Instruction::SET_DYNAMIC_TABLE_CAPACITY == encoder::Instruction::SET_DYNAMIC_TABLE_CAPACITY {
                let (output, input) = Decoder::dynamic_table_capacity(wire, idx)?;
                commit_funcs.push(self.table.set_dynamic_table_capacity(input)?);
                output
            } else { // if wire[idx] & encoder::Instruction::DUPLICATE == encoder::Instruction::DUPLICATE
                let (output, input) = Decoder::duplicate(wire, idx)?;
                commit_funcs.push(self.table.duplicate(input)?);
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

    pub fn decode_decoder_instruction(&self, wire: &Vec<u8>)
        -> Result<Box<dyn FnOnce() -> Result<(), Box<dyn error::Error>>>,
                    Box<dyn error::Error>> {
        let mut idx = 0;
        let wire_len = wire.len();
        let mut commit_funcs: Vec<Box<dyn FnOnce() -> Result<(), Box<dyn error::Error>>>> = vec![];

        while idx < wire_len {
            idx += if wire[idx] & decoder::Instruction::SECTION_ACKNOWLEDGMENT == decoder::Instruction::SECTION_ACKNOWLEDGMENT {
                let (len, stream_id) = Encoder::section_ackowledgment(wire, idx)?;
                if !self.encoder.read().unwrap().has_section(stream_id) {
                    // section has already been acked
                    return Err(DecoderStreamError.into());
                }
                let encoder_c = Arc::clone(&self.encoder);
                let dynamic_table = Arc::clone(&self.table.dynamic_table);
                commit_funcs.push(Box::new(move || -> Result<(), Box<dyn error::Error>> {
                    let section = encoder_c.write().unwrap().ack_section(stream_id);
                    dynamic_table.write().unwrap().ack_section(section);
                    Ok(())
                }));
                len
            } else if wire[idx] & decoder::Instruction::STREAM_CANCELLATION == decoder::Instruction::STREAM_CANCELLATION {
                let (len, stream_id) = Encoder::stream_cancellation(wire, idx)?;
                let encoder_c = Arc::clone(&self.encoder);
                commit_funcs.push(Box::new(move || -> Result<(), Box<dyn error::Error>> {
                    encoder_c.write().unwrap().cancel_section(stream_id);
                    Ok(())
                }));
                len
            } else { // wire[idx] & Instruction::INSERT_COUNT_INCREMENT == Instruction::INSERT_COUNT_INCREMENT
                let (len, increment) = Encoder::insert_count_increment(wire, idx)?;
                if increment == 0 || *self.encoder.read().unwrap().known_sending_count.read().unwrap() < self.table.dynamic_table.read().unwrap().known_received_count + increment {
                    return Err(DecoderStreamError.into());
                }
                let dynamic_table = Arc::clone(&self.table.dynamic_table);
                commit_funcs.push(Box::new(move || -> Result<(), Box<dyn error::Error>> {
                    dynamic_table.write().unwrap().known_received_count += increment;
                    Ok(())
                }));
                len
            };
        }
        Ok(Box::new(move || -> Result<(), Box<dyn error::Error>> {
            for f in commit_funcs {
                f()?;
            }
            Ok(())
        }))
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
		let qpack = Qpack::new(1, 1024);
		let headers = vec![Header::from(":path", "/index.html")];
		let mut encoded = vec![];
		let commit_func = qpack.encode_headers(&mut encoded, false, headers, STREAM_ID);
		let out = commit_func.unwrap()();
		assert_eq!(out.unwrap(), ());
		assert_eq!(encoded,
					vec![0x00, 0x00, 0x51, 0x0b, 0x2f,
						 0x69, 0x6e, 0x64, 0x65, 0x78,
						 0x2e, 0x68, 0x74, 0x6d, 0x6c]);
	}
	#[test]
	fn rfc_appendix_b1_decode() {
		let qpack = Qpack::new(1, 1024);
		let wire = vec![0x00, 0x00, 0x51, 0x0b, 0x2f,
								0x69, 0x6e, 0x64, 0x65, 0x78,
								0x2e, 0x68, 0x74, 0x6d, 0x6c];
		let out = qpack.decode_headers(&wire, STREAM_ID).unwrap();
		assert_eq!(out.0, vec![Header::from(":path", "/index.html")]);
		assert_eq!(out.1, false);
	}

	#[test]
	fn encode_indexed_simple() {
		let qpack = Qpack::new(1, 1024);
		let headers = vec![Header::from(":path", "/")];
        let mut encoded = vec![];
		let commit_func = qpack.encode_headers(&mut encoded, false, headers, STREAM_ID);
		let out = commit_func.unwrap()();
		assert_eq!(out.unwrap(), ());
		assert_eq!(encoded,
			vec![0x00, 0x00, 0xc1]);
	}
	#[test]
	fn decode_indexed_simple() {
		let qpack = Qpack::new(1, 1024);
		let wire = vec![0x00, 0x00, 0xc1];
		let out = qpack.decode_headers(&wire, STREAM_ID).unwrap();
		assert_eq!(out.0,
			vec![Header::from(":path", "/")]);
        assert_eq!(out.1, false);
	}
    #[test]
    fn encode_set_dynamic_table_capacity() {
        let qpack = Qpack::new(1, 1024);
        let mut encoded = vec![];
        let _ = qpack.encode_set_dynamic_table_capacity(&mut encoded, 220);
        assert_eq!(encoded, vec![0x3f, 0xbd, 0x01]);
    }
    #[test]
    fn multi_threading() {
        let qpack_encoder = Qpack::new(2, 1024);
        let qpack_decoder = Qpack::new(2, 1024);
        let safe_encoder = Arc::new(RwLock::new(qpack_encoder));
        let safe_decoder = Arc::new(RwLock::new(qpack_decoder));

        let f = |headers: Vec<Header>, stream_id: u16, expected_wire: Vec<u8>,
                                                encoder: Arc<RwLock<Qpack>>, decoder: Arc<RwLock<Qpack>>| -> Result<(), Box<dyn error::Error>> {
            let mut encoded = vec![];
            let commit_func = encoder.read().unwrap().encode_headers(&mut encoded, false, headers.clone(), stream_id)?;
            let out = commit_func();
            assert_eq!(out.unwrap(), ());
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
        let qpack_encoder = Qpack::new(1, 1024);
        let qpack_decoder = Qpack::new(1, 1024);

        println!("Step 1");
        {   // encoder instruction
            let mut encoded = vec![];
            let capacity = 220;
            let commit_func = qpack_encoder.encode_set_dynamic_table_capacity(&mut encoded, capacity);
            let out = commit_func.unwrap()();
            assert_eq!(out.unwrap(), ());
            let headers = vec![Header::from(":authority", "www.example.com"),
                                          Header::from(":path", "/sample/path")];

            let commit_func = qpack_encoder.encode_insert_headers(&mut encoded, headers);
            assert_eq!(encoded, vec![0x3f, 0xbd, 0x01, 0xc0, 0x0f, 0x77, 0x77,
                                    0x77, 0x2e, 0x65, 0x78, 0x61, 0x6d, 0x70,
                                    0x6c, 0x65, 0x2e, 0x63, 0x6f, 0x6d, 0xc1,
                                    0x0c, 0x2f, 0x73, 0x61, 0x6d, 0x70, 0x6c,
                                    0x65, 0x2f, 0x70, 0x61, 0x74, 0x68]);
            let out = commit_func.unwrap()();
            assert_eq!(out.unwrap(), ());

            let commit_func = qpack_decoder.decode_encoder_instruction(&encoded);
            let out = commit_func.unwrap()();
            assert_eq!(out.unwrap(), ());

            qpack_encoder.dump_dynamic_table();
            qpack_decoder.dump_dynamic_table();
        }

        println!("Step 2");
        {   // header transfer
            let mut encoded = vec![];
            let headers = vec![Header::from(":authority", "www.example.com"),
                                          Header::from(":path", "/sample/path")];
            let commit_func = qpack_encoder.encode_headers(&mut encoded, true, headers.clone(), STREAM_ID);
            let out = commit_func.unwrap()();
            assert_eq!(out.unwrap(), ());
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
            let commit_func = qpack_decoder.encode_section_ackowledgment(&mut encoded, STREAM_ID);
            assert_eq!(encoded, vec![0x84]);
            let out = commit_func.unwrap()();
            assert_eq!(out.unwrap(), ());

            let commit_func = qpack_encoder.decode_decoder_instruction(&encoded);
            let out = commit_func.unwrap()();
            assert_eq!(out.unwrap(), ());
            println!("dump encoder side dynamic table");
            qpack_encoder.dump_dynamic_table();
            println!("dump decoder side dynamic table");
            qpack_decoder.dump_dynamic_table();
        }

        println!("Step 4");
        {   // encoder instruction
            let mut encoded = vec![];
            let headers = vec![Header::from("custom-key", "custom-value")];
            let commit_func = qpack_encoder.encode_insert_headers(&mut encoded, headers);
            assert_eq!(encoded, vec![0x4a, 0x63, 0x75, 0x73, 0x74, 0x6f, 0x6d, 0x2d, 0x6b, 0x65,
                                    0x79, 0x0c, 0x63, 0x75, 0x73, 0x74, 0x6f, 0x6d, 0x2d, 0x76,
                                    0x61, 0x6c, 0x75, 0x65]);
            let out = commit_func.unwrap()();
            assert_eq!(out.unwrap(), ());

            let commit_func = qpack_decoder.decode_encoder_instruction(&encoded);
            let out = commit_func.unwrap()();
            assert_eq!(out.unwrap(), ());

            println!("dump encoder side dynamic table");
            qpack_encoder.dump_dynamic_table();
            println!("dump decoder side dynamic table");
            qpack_decoder.dump_dynamic_table();
        }

        println!("Step 5");
        {   // decoder instruction
            let mut encoded = vec![];
            let commit_func = qpack_decoder.encode_insert_count_increment(&mut encoded);
            assert_eq!(encoded, vec![0x01]);
            let out = commit_func.unwrap()();
            assert_eq!(out.unwrap(), ());

            let commit_func = qpack_encoder.decode_decoder_instruction(&encoded);
            let out = commit_func.unwrap()();
            assert_eq!(out.unwrap(), ());

            qpack_encoder.dump_dynamic_table();
            qpack_decoder.dump_dynamic_table();
        }

        println!("Step 6");
        {   // encoder instruction
            let mut encoded = vec![];
            let headers = vec![Header::from(":authority", "www.example.com")];
            let commit_func = qpack_encoder.encode_insert_headers(&mut encoded, headers);
            assert_eq!(encoded, vec![0x02]);
            let out = commit_func.unwrap()();
            assert_eq!(out.unwrap(), ());

            let commit_func = qpack_decoder.decode_encoder_instruction(&encoded);
            let out = commit_func.unwrap()();
            assert_eq!(out.unwrap(), ());

            qpack_encoder.dump_dynamic_table();
            qpack_decoder.dump_dynamic_table();
        }

        println!("Step 7");
        {   // header transfer
            let mut encoded = vec![];
            let headers = vec![Header::from(":authority", "www.example.com"),
                                        Header::from(":path", "/"),
                                        Header::from("custom-key", "custom-value")];
            let commit_func = qpack_encoder.encode_headers(&mut encoded, false, headers.clone(), 8);
            let out = commit_func.unwrap()();
            assert_eq!(out.unwrap(), ());
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
            let commit_func = qpack_decoder.encode_stream_cancellation(&mut encoded, 8);
            assert_eq!(encoded, vec![0x48]);
            let out = commit_func.unwrap()();
            assert_eq!(out.unwrap(), ());

            let commit_func = qpack_encoder.decode_decoder_instruction(&encoded);
            let out = commit_func.unwrap()();
            assert_eq!(out.unwrap(), ());
        }

        println!("Step 9");
        {   // encoder instruction
            let mut encoded = vec![];
            let headers = vec![Header::from("custom-key", "custom-value2")];
            let commit_func = qpack_encoder.encode_insert_headers(&mut encoded, headers);
            assert_eq!(encoded, vec![0x81, 0x0d, 0x63, 0x75, 0x73, 0x74, 0x6f, 0x6d,
                                     0x2d, 0x76, 0x61, 0x6c, 0x75, 0x65, 0x32]);
            let out = commit_func.unwrap()();
            assert_eq!(out.unwrap(), ());

            let commit_func = qpack_decoder.decode_encoder_instruction(&encoded);
            let _ = commit_func.unwrap()();

            println!("dump encoder side dynamic table");
            qpack_encoder.dump_dynamic_table();
            println!("dump decoder side dynamic table");
            qpack_decoder.dump_dynamic_table();
        }
    }
}