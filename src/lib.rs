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

    pub fn encode(&mut self, headers: Vec<Header>, dynamic_table_cap: Option<u32>, use_static: bool) -> Vec<u8> {
        self.encoder.encode(&mut self.table, headers, dynamic_table_cap, use_static)
    }
    pub fn decode<'a>(&'a mut self, wire: &'a Vec<u8>) -> Result<Vec<Header>, Box<dyn error::Error>> {
        self.decoder.decode(wire, &mut self.table)
    }
}

struct Qnum;
impl Qnum {
    pub fn encode(encoded: &mut Vec<u8>, val: u32, N: u8) -> usize {
		let mut val = val;
        let mut len = 1;
        if val < (1 << N) - 1 {
            encoded.push(val as u8);
            return len;
        }
        encoded.push((1 << N) - 1);
        val -= (1 << N) - 1;
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
    pub const Indexed: u8 = 0b10000000;
    // 4.5.3
    // 0   1   2   3   4   5   6   7
    // +---+---+---+---+---+---+---+---+
    // | 0 | 0 | 0 | 1 |  Index (4+)   |
    // +---+---+---+---+---------------+
    pub const IndexedPostBaseIndex: u8 = 0b00010000;
    // 4.5.4
    // 0   1   2   3   4   5   6   7
    // +---+---+---+---+---+---+---+---+
    // | 0 | 1 | N | T |Name Index (4+)|
    // +---+---+---+---+---------------+
    // | H |     Value Length (7+)     |
    // +---+---------------------------+
    // |  Value String (Length bytes)  |
    // +-------------------------------+
    pub const LiteralNameReference: u8 = 0b01000000;
    // 4.5.5
    // 0   1   2   3   4   5   6   7
    // +---+---+---+---+---+---+---+---+
    // | 0 | 0 | 0 | 0 | N |NameIdx(3+)|
    // +---+---+---+---+---+-----------+
    // | H |     Value Length (7+)     |
    // +---+---------------------------+
    // |  Value String (Length bytes)  |
    // +-------------------------------+
    pub const LiteralPostBaseNameReference: u8 = 0b00000000;
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
    pub const LiteralLiteralName: u8 = 0b00100000;
}

struct Setting;

impl Setting {
    pub const QPACK_MAX_TABLE_CAPACITY: u8 = 0x01;
    pub const QPACK_BLOCKED_STREAMS: u8 = 0x07;
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

struct Stream;

impl Stream {
    pub const QPACK_ENCODER_STREAM: u8 = 0x02;
    pub const QPACK_DECODER_STREAM: u8 = 0x03;
}

// reference doesn't work for decoding?
//type Header = (&'static str, &'static str);
type Header<'a> = (&'a str, &'a str);

#[cfg(test)]
mod tests {
	use crate::Qpack;
	#[test]
	fn rfc_appendix_b1_encode() {
		let mut qpack = Qpack::new(1024);
		let headers = vec![(":path", "/index.html")];
		let out = qpack.encode(headers, None, true);
		assert_eq!(out,
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
		let out = qpack.decode(&wire).unwrap();
		assert_eq!(out, vec![(":path", "/index.html")]);
	}

	#[test]
	fn encode_indexed_simple() {
		let mut qpack = Qpack::new(1024);
		let headers = vec![(":path", "/")];
		let out = qpack.encode(headers, None, true);
		assert_eq!(out,
			vec![0x00, 0x00, 0xc1]);
	}
	#[test]
	fn decode_indexed_simple() {
		let mut qpack = Qpack::new(1024);
		let wire = vec![0x00, 0x00, 0xc1];
		let out = qpack.decode(&wire).unwrap();
		assert_eq!(out,
			vec![(":path", "/")]);
	}
}