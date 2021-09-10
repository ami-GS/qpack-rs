mod transformer;
mod table;
mod types;

use types::{CommitFunc, Header};
use crate::transformer::decoder::{self, Decoder};
use crate::transformer::encoder::{self, Encoder};
use crate::table::Table;
use core::fmt;
use std::error;
use std::sync::{Arc, Condvar, Mutex, RwLock};
#[macro_use]
extern crate lazy_static;

pub struct Qpack {
    encoder: Arc<RwLock<Encoder>>,
    decoder: Arc<RwLock<Decoder>>,
    table: Table,
    blocked_streams_limit: u16,
    cv_insert_count: Arc<(Mutex<usize>, Condvar)>,
}

impl Qpack {
    pub fn new(blocked_streams_limit: u16, dynamic_table_max_capacity: usize) -> Self {
        let cv_insert_count = Arc::new((Mutex::new(0), Condvar::new()));
        Qpack {
            encoder: Arc::new(RwLock::new(Encoder::new())),
            decoder: Arc::new(RwLock::new(Decoder::new())),
            table: Table::new(dynamic_table_max_capacity, Arc::clone(&cv_insert_count)),
            blocked_streams_limit,
            cv_insert_count,
        }
    }
    pub fn is_insertable(&self, headers: &Vec<Header>) -> bool {
        self.table.is_insertable(headers)
    }
    pub fn encode_insert_headers(&self, encoded: &mut Vec<u8>, headers: Vec<Header>)
            -> Result<CommitFunc, Box<dyn error::Error>> {
        let mut commit_funcs = vec![];
        // INFO: Perforamnce of bulk lookup or lookup each would be depends on lookup algorithm
        let find_index_results = self.table.find_headers(&headers);
        for (i, header)  in headers.into_iter().enumerate() {
            let (both_match, on_static, mut idx) = find_index_results[i];
            if idx != usize::MAX && !on_static {
                // absolute to relative (against 0) conversion
                idx = self.table.get_insert_count() - 1 - idx
            }

            if both_match && !on_static {
                Encoder::encode_duplicate(encoded, idx)?;
                commit_funcs.push(self.table.duplicate(idx)?);
            } else if idx != usize::MAX {
                let value = header.move_value();
                Encoder::encode_insert_refer_name(encoded, on_static, idx, &value)?;
                commit_funcs.push(self.table.insert_refer_name(idx, value, on_static)?);
            } else {
                Encoder::encode_insert_both_literal(encoded, &header)?;
                commit_funcs.push(self.table.insert_both_literal(header)?);
            }
        }

        let encoder = Arc::clone(&self.encoder);
        let dynamic_table = Arc::clone(&self.table.dynamic_table);
        Ok(Box::new(move || -> Result<(), Box<dyn error::Error>> {
            let count = commit_funcs.len();
            let mut locked_table = dynamic_table.write().unwrap();
            commit_funcs.into_iter().try_for_each(|f| f(&mut locked_table))?;
            encoder.write().unwrap().known_sending_count += count;
            Ok(())
        }))
    }
    pub fn encode_set_dynamic_table_capacity(&self, encoded: &mut Vec<u8>, capacity: usize)
            -> Result<CommitFunc, Box<dyn error::Error>> {
        Encoder::encode_set_dynamic_table_capacity(encoded, capacity)?;
        let dynamic_table = Arc::clone(&self.table.dynamic_table);
        Ok(Box::new(move || -> Result<(), Box<dyn error::Error>> {
            dynamic_table.write().unwrap().set_capacity(capacity)
        }))
    }
    pub fn encode_section_ackowledgment(&self, encoded: &mut Vec<u8>, stream_id: u16)
            -> Result<CommitFunc, Box<dyn error::Error>> {
        Decoder::encode_section_ackowledgment(encoded, stream_id)?;
        let decoder = Arc::clone(&self.decoder);
        let dynamic_table = Arc::clone(&self.table.dynamic_table);
        Ok(Box::new(move || -> Result<(), Box<dyn error::Error>> {
            let section = decoder.write().unwrap().ack_section(stream_id);
            dynamic_table.write().unwrap().ack_section(section, vec![]);
            Ok(())
        }))
    }
    pub fn encode_stream_cancellation(&self, encoded: &mut Vec<u8>, stream_id: u16)
            -> Result<CommitFunc, Box<dyn error::Error>> {
        Decoder::encode_stream_cancellation(encoded, stream_id)?;
        let decoder = Arc::clone(&self.decoder);
        Ok(Box::new(move || -> Result<(), Box<dyn error::Error>> {
            decoder.write().unwrap().cancel_section(stream_id);
            Ok(())
        }))
    }
    // TODO: check whether to update state
    pub fn encode_insert_count_increment(&self, encoded: &mut Vec<u8>)
            -> Result<CommitFunc, Box<dyn error::Error>> {
        let dynamic_table_read = self.table.dynamic_table.read().unwrap();
        let increment = dynamic_table_read.list.len() - dynamic_table_read.known_received_count;
        Decoder::encode_insert_count_increment(encoded, increment)?;
        let dynamic_table = Arc::clone(&self.table.dynamic_table);
        Ok(Box::new(move || -> Result<(), Box<dyn error::Error>> {
            dynamic_table.write().unwrap().known_received_count += increment;
            Ok(())
        }))
    }

    fn get_prefix_meta_data(&self, find_index_results: &Vec<(bool, bool, usize)>, refer_dynamic_table: bool) -> (usize, bool, u32) {
        if !refer_dynamic_table {
            return (0, false, 0);
        }
        // if same distribusion, then post base.
        // currently just range
        let mut min_max = (usize::MAX, usize::MIN);
        for i in 0..find_index_results.len() {
            let result = &find_index_results[i];
            if result.1 || result.2 == usize::MAX {
                continue;
            }
            if result.2 < min_max.0 {
                min_max.0 = result.2;
            }
            if min_max.1 < result.2 {
                min_max.1 = result.2;
            }
        }
        if min_max == (usize::MAX, usize::MIN) {
            return (0, false, 0);
        }
        let insert_count = self.table.get_insert_count();
        let entry_len = self.table.get_dynamic_table_entry_len();
        let evicted_count = insert_count - entry_len;

        let required_insert_count = min_max.1 + 1 + evicted_count;
        // WARN: if min_max uses abs_index, entry_len to be insert_count
        let post_base = ((min_max.0 + min_max.1) / 2) < entry_len / 2;
        (
            required_insert_count,
            post_base,
            if post_base {min_max.0} else {required_insert_count} as u32
        )
    }

    pub fn encode_headers(&self, encoded: &mut Vec<u8>, headers: Vec<Header>, stream_id: u16)
            -> Result<CommitFunc, Box<dyn error::Error>> {
        let find_index_results = self.table.find_headers(&headers);
        let (required_insert_count, post_base, base) = self.get_prefix_meta_data(&find_index_results, true);
        Encoder::prefix(encoded,
                        &self.table,
                        required_insert_count as u32,
                        post_base,
                        base);

        let mut dynamic_table_indices = vec![];
        for (i, header) in headers.into_iter().enumerate() {
            let (both_match, on_static, idx) = find_index_results[i];
            if !on_static && idx != usize::MAX {
                dynamic_table_indices.push(idx);
            }

            if both_match && !header.sensitive {
                if on_static {
                    Encoder::encode_indexed(encoded, idx as u32, true);
                } else {
                    if post_base {
                        Encoder::encode_indexed_post_base(encoded, idx as u32 - base);
                    } else {
                        Encoder::encode_indexed(encoded, base - idx as u32 - 1, false);
                    }
                }
            } else if idx != usize::MAX {
                if on_static {
                    Encoder::encode_refer_name(encoded, idx as u32, header, true)?;
                } else {
                    if post_base {
                        Encoder::encode_refer_name_post_base(encoded, idx as u32 - base, header)?;
                    } else {
                        Encoder::encode_refer_name(encoded, base - idx as u32 - 1, header, false)?;
                    }
                }
            } else { // not found
                Encoder::encode_both_literal(encoded, header)?;
            }
        }
        let encoder = Arc::clone(&self.encoder);
        let dynamic_table = Arc::clone(&self.table.dynamic_table);
        Ok(Box::new(move || -> Result<(), Box<dyn error::Error>> {
            if 0 < dynamic_table_indices.len() {
                let mut write_lock = dynamic_table.write().unwrap();
                dynamic_table_indices.iter().try_for_each(|idx| write_lock.ref_entry_at(*idx))?;
                encoder.write().unwrap().add_section(stream_id, required_insert_count, dynamic_table_indices);
            }
            Ok(())
        }))
    }

    fn block_decoding(&self, required_insert_count: usize) -> Result<(), Box<dyn error::Error>> {
        if self.blocked_streams_limit < self.decoder.read().unwrap().current_blocked_streams + 1 {
            return Err(DecompressionFailed.into());
        }
        self.decoder.write().unwrap().current_blocked_streams += 1;

        let (mux, cv) = &*self.cv_insert_count;

        let locked_insert_count = mux.lock().unwrap();
        let _ = cv.wait_while(locked_insert_count, |locked_insert_count| *locked_insert_count < required_insert_count).unwrap();
        Ok(())
    }
    pub fn decode_headers(&self, wire: &Vec<u8>, stream_id: u16) -> Result<(Vec<Header>, bool), Box<dyn error::Error>> {
        let mut idx = 0;
        let (len, required_insert_count, base) = Decoder::prefix(wire, idx, &self.table)?;
        idx += len;
        let required_insert_count = required_insert_count as usize;

        // blocked if dynamic_table.insert_count < requred_insert_count
        // OPTIMIZE: blocked just before referencing dynamic_table is better?
        let insert_count = self.table.get_insert_count();
        if insert_count < required_insert_count {
            self.block_decoding(required_insert_count)?;
        }

        let mut headers = vec![];
        let wire_len = wire.len();
        let mut ref_dynamic = false;
        while idx < wire_len {
            let ret = if wire[idx] & FieldType::INDEXED == FieldType::INDEXED {
                Decoder::decode_indexed(wire, &mut idx, base, required_insert_count, &self.table)?
            } else if wire[idx] & FieldType::REFER_NAME == FieldType::REFER_NAME {
                Decoder::decode_refer_name(wire, &mut idx, base, required_insert_count, &self.table)?
            } else if wire[idx] & FieldType::BOTH_LITERAL == FieldType::BOTH_LITERAL {
                Decoder::decode_both_literal(wire, &mut idx)?
            } else if wire[idx] & FieldType::INDEXED_POST_BASE == FieldType::INDEXED_POST_BASE {
                Decoder::decode_indexed_post_base(wire, &mut idx, base, required_insert_count, &self.table)?
            } else if wire[idx] & 0b11110000 == FieldType::REFER_NAME_POST_BASE {
                Decoder::decode_refer_name_post_base(wire, &mut idx, base, required_insert_count, &self.table)?
            } else {
                return Err(DecompressionFailed.into());
            };
            headers.push(ret.0);
            ref_dynamic |= ret.1;
        }
        // ?
        // TODO: move to commit func?
        if required_insert_count != 0 {
            self.decoder.write().unwrap().add_section(stream_id, required_insert_count);
        }
        Ok((headers, ref_dynamic))
    }
    pub fn decode_encoder_instruction(&self, wire: &Vec<u8>)
            -> Result<CommitFunc, Box<dyn error::Error>> {
        let mut idx = 0;
        let wire_len = wire.len();
        let mut commit_funcs = vec![];

        while idx < wire_len {
            idx += if wire[idx] & encoder::Instruction::INSERT_REFER_NAME == encoder::Instruction::INSERT_REFER_NAME {
                let (output, input) = Decoder::decode_insert_refer_name(wire, idx)?;
                commit_funcs.push(self.table.insert_refer_name(input.0, input.1, input.2)?);
                output
            } else if wire[idx] & encoder::Instruction::INSERT_BOTH_LITERAL == encoder::Instruction::INSERT_BOTH_LITERAL {
                let (output, input) = Decoder::decode_insert_both_literal(wire, idx)?;
                commit_funcs.push(self.table.insert_both_literal(input)?);
                output
            } else if wire[idx] & encoder::Instruction::SET_DYNAMIC_TABLE_CAPACITY == encoder::Instruction::SET_DYNAMIC_TABLE_CAPACITY {
                let (output, input) = Decoder::decode_dynamic_table_capacity(wire, idx)?;
                commit_funcs.push(self.table.set_dynamic_table_capacity(input)?);
                output
            } else { // if wire[idx] & encoder::Instruction::DUPLICATE == encoder::Instruction::DUPLICATE
                let (output, input) = Decoder::decode_duplicate(wire, idx)?;
                commit_funcs.push(self.table.duplicate(input)?);
                output
            };
        }
        let dynamic_table = Arc::clone(&self.table.dynamic_table);
        Ok(Box::new(move || -> Result<(), Box<dyn error::Error>> {
            let mut locked_table = dynamic_table.write().unwrap();
            commit_funcs.into_iter().try_for_each(|f| f(&mut locked_table))?;
            Ok(())
        }))
    }

    pub fn decode_decoder_instruction(&self, wire: &Vec<u8>)
            -> Result<CommitFunc, Box<dyn error::Error>> {
        let mut idx = 0;
        let wire_len = wire.len();
        let mut commit_funcs = vec![];

        while idx < wire_len {
            idx += if wire[idx] & decoder::Instruction::SECTION_ACKNOWLEDGMENT == decoder::Instruction::SECTION_ACKNOWLEDGMENT {
                let (len, stream_id) = Encoder::decode_section_ackowledgment(wire, idx)?;
                if !self.encoder.read().unwrap().has_section(stream_id) {
                    // $4.4.1 section has already been acked
                    return Err(DecoderStreamError.into());
                }
                commit_funcs.push(self.table.section_ackowledgment(Arc::clone(&self.encoder), stream_id)?);
                len
            } else if wire[idx] & decoder::Instruction::STREAM_CANCELLATION == decoder::Instruction::STREAM_CANCELLATION {
                let (len, stream_id) = Encoder::decode_stream_cancellation(wire, idx)?;
                commit_funcs.push(self.table.stream_cancellation(Arc::clone(&self.encoder), stream_id)?);
                len
            } else { // wire[idx] & Instruction::INSERT_COUNT_INCREMENT == Instruction::INSERT_COUNT_INCREMENT
                let (len, increment) = Encoder::decode_insert_count_increment(wire, idx)?;
                if increment == 0 || self.encoder.read().unwrap().known_sending_count < self.table.dynamic_table.read().unwrap().known_received_count + increment {
                    // 4.4.3 invalid value
                    return Err(DecoderStreamError.into());
                }
                commit_funcs.push(self.table.insert_count_increment(increment)?);
                len
            };
        }
        let dynamic_table = Arc::clone(&self.table.dynamic_table);
        Ok(Box::new(move || -> Result<(), Box<dyn error::Error>> {
            let mut locked_dynamic_table = dynamic_table.write().unwrap();
            commit_funcs.into_iter().try_for_each(|f| f(&mut locked_dynamic_table))?;
            Ok(())
        }))
    }
    pub fn dump_dynamic_table(&self) {
        self.table.dump_dynamic_table();
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
    pub const INDEXED_POST_BASE: u8 = 0b00010000;
    // 4.5.4
    // 0   1   2   3   4   5   6   7
    // +---+---+---+---+---+---+---+---+
    // | 0 | 1 | N | T |Name Index (4+)|
    // +---+---+---+---+---------------+
    // | H |     Value Length (7+)     |
    // +---+---------------------------+
    // |  Value String (Length bytes)  |
    // +-------------------------------+
    pub const REFER_NAME: u8 = 0b01000000;
    // 4.5.5
    // 0   1   2   3   4   5   6   7
    // +---+---+---+---+---+---+---+---+
    // | 0 | 0 | 0 | 0 | N |NameIdx(3+)|
    // +---+---+---+---+---+-----------+
    // | H |     Value Length (7+)     |
    // +---+---------------------------+
    // |  Value String (Length bytes)  |
    // +-------------------------------+
    pub const REFER_NAME_POST_BASE: u8 = 0b00000000;
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
    pub const BOTH_LITERAL: u8 = 0b00100000;
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

#[cfg(test)]
mod tests {
    use core::time;
    use std::{error, sync::Arc, thread};
    use crate::{Header, Qpack, types::HeaderString};

    static STREAM_ID: u16 = 4;
    fn get_request_headers(remove_value: bool) -> Vec<Header> {
        let mut headers = vec![
            Header::from_str(":authority", "example.com"),
            Header::from_str(":method", "GET"),
            Header::from_str(":path", "/"),
            Header::from_str(":scheme", "https"),
            Header::from_str("accept", "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.9"),
            Header::from_str("accept-encoding", "gzip, deflate, br"),
            Header::from_str("accept-language", "en-US,en;q=0.9"),
            Header::from_str("sec-ch-ua", "\"Chromium\";v=\"92\", \" Not A;Brand\";v=\"99\", \"Google Chrome\";v=\"92\""),
            Header::from_str("sec-ch-ua-mobile", "?0"),
            Header::from_str("sec-fetch-dest", "document"),
            Header::from_str("sec-fetch-mode", "navigate"),
            Header::from_str("sec-fetch-site", "none"),
            Header::from_str("sec-fetch-user", "?1"),
            Header::from_str("upgrade-insecure-requests", "1"),
            Header::from_str("user-agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/92.0.4515.107 Safari/537.36")
        ];
        if remove_value {
            for header in headers.iter_mut() {
                header.set_value(HeaderString::new("".to_string(), false));
            }
        }
        headers
    }

    fn get_response_headers(remove_value: bool) -> Vec<Header> {
        let mut headers = vec![
            Header::from_str("age", "316723"),
            Header::from_str("cache-control", "max-age=604800"),
            Header::from_str("content-encoding", "gzip"),
            Header::from_str("content-length", "648"),
            Header::from_str("content-type", "text/html; charset=UTF-8"),
            Header::from_str("date", "Tue, 10 Aug 2021 06:59:14 GMT"),
            Header::from_str("etag", "\"3147526947+ident+gzip\""),
            Header::from_str("expires", "Tue, 17 Aug 2021 06:59:14 GMT"),
            Header::from_str("last-modified", "Thu, 17 Oct 2019 07:18:26 GMT"),
            Header::from_str("server", "ECS (sab/5708)"),
            Header::from_str("vary", "Accept-Encoding"),
            Header::from_str("x-cache", "HIT"),
        ];
        if remove_value {
            for header in headers.iter_mut() {
                header.set_value(HeaderString::new("".to_string(), false));
            }
        }
        headers
    }

    fn commit(func: Result<Box<dyn FnOnce() -> Result<(), Box<dyn error::Error>>>,
                            Box<dyn error::Error>>) {
        match func {
            Ok(ok) => {
                match ok() {
                    Ok(_) => (),
                    Err(e) => {
                        println!("{:?}", e);
                        assert!(false);
                        ()
                    },
                }
            },
            Err(e) => {
                println!("{:?}", e);
                assert!(false);
            },
        }
    }

    fn set_table_capacity(client: &Qpack, server: &Qpack, table_size: usize) {
        let mut encoded = vec![];
        let commit_func = client.encode_set_dynamic_table_capacity(&mut encoded, table_size);
        commit(commit_func);
        let commit_func = server.decode_encoder_instruction(&encoded);
        commit(commit_func);
    }
    fn insert_headers(client: &Qpack, server: &Qpack, headers: Vec<Header>) {
        if !client.is_insertable(&headers) {
            assert!(false);
        }
        let mut encoded = vec![];
        let commit_func = client.encode_insert_headers(&mut encoded, headers);
        commit(commit_func);
        let commit_func = server.decode_encoder_instruction(&encoded);
        commit(commit_func);
    }
    fn send_headers(client: &Qpack, server: &Qpack, headers: Vec<Header>) -> bool {
        let mut encoded = vec![];
        let commit_func = client.encode_headers(&mut encoded, headers.clone(), STREAM_ID);
        commit(commit_func);
        let out = server.decode_headers(&encoded, STREAM_ID).unwrap();
        assert_eq!(headers, out.0);
        out.1
    }
    fn section_ackowledgment(client: &Qpack, server: &Qpack) {
        let mut encoded = vec![];
        let commit_func = server.encode_section_ackowledgment(&mut encoded, STREAM_ID);
        commit(commit_func);
        let commit_func = client.decode_decoder_instruction(&encoded);
        commit(commit_func);
    }

    fn gen_client_server_instances(blocked_streams_limit: u16, table_size: usize) -> (Qpack, Qpack) {
        let qpack_client = Qpack::new(blocked_streams_limit, table_size);
        let qpack_server = Qpack::new(blocked_streams_limit, table_size);
        set_table_capacity(&qpack_client, &qpack_server, table_size);
        (qpack_client, qpack_server)
    }

    fn insert_send_ack(encoder: &Qpack, decoder: &Qpack, headers: Vec<Header>, dump_table: bool) {
        insert_headers(encoder, decoder, headers.clone());
        let refer_dynamic_table = send_headers(encoder, decoder, headers.clone());
        if refer_dynamic_table {
            section_ackowledgment(encoder, decoder);
        }
        if dump_table {
            encoder.dump_dynamic_table();
            decoder.dump_dynamic_table();
        }
    }

    #[test]
    fn simple_get() {
        let (qpack_encoder, qpack_decoder) = gen_client_server_instances(1, 1024);
        let request_headers = get_request_headers(false);
        let refer_dynamic_table = send_headers(&qpack_encoder, &qpack_decoder, request_headers.clone());
        assert!(!refer_dynamic_table);
    }

    #[test]
    fn simple_get_huffman() {
        let (qpack_encoder, qpack_decoder) = gen_client_server_instances(1, 1024);
        let mut request_headers = get_request_headers(false);
        request_headers.iter_mut().for_each(|header| header.set_huffman((true, true)));
        let refer_dynamic_table = send_headers(&qpack_encoder, &qpack_decoder, request_headers.clone());
        assert!(!refer_dynamic_table);

        request_headers.iter_mut().for_each(|header| header.set_huffman((true, false)));
        let refer_dynamic_table = send_headers(&qpack_encoder, &qpack_decoder, request_headers.clone());
        assert!(!refer_dynamic_table);

        request_headers.iter_mut().for_each(|header| header.set_huffman((false, true)));
        let refer_dynamic_table = send_headers(&qpack_encoder, &qpack_decoder, request_headers.clone());
        assert!(!refer_dynamic_table);
    }

    #[test]
    fn simple_get_sensitive() {
        let (qpack_encoder, qpack_decoder) = gen_client_server_instances(1, 1024);
        let mut request_headers = get_request_headers(false);
        request_headers.iter_mut().for_each(|header| header.set_sensitive(true));
        let refer_dynamic_table = send_headers(&qpack_encoder, &qpack_decoder, request_headers);
        assert!(!refer_dynamic_table);
    }

    #[test]
    fn simple_get_huffman_sensitive() {
        let (qpack_encoder, qpack_decoder) = gen_client_server_instances(1, 1024);
        let mut request_headers = get_request_headers(false);
        request_headers.iter_mut().for_each(|header| header.set_sensitive(true));
        request_headers.iter_mut().for_each(|header| header.set_huffman((true, true)));
        let refer_dynamic_table = send_headers(&qpack_encoder, &qpack_decoder, request_headers.clone());
        assert!(!refer_dynamic_table);

        request_headers.iter_mut().for_each(|header| header.set_huffman((true, false)));
        let refer_dynamic_table = send_headers(&qpack_encoder, &qpack_decoder, request_headers.clone());
        assert!(!refer_dynamic_table);

        request_headers.iter_mut().for_each(|header| header.set_huffman((false, true)));
        let refer_dynamic_table = send_headers(&qpack_encoder, &qpack_decoder, request_headers.clone());
        assert!(!refer_dynamic_table);

    }

    #[test]
    fn insert_simple_headers() {
        let (qpack_encoder, qpack_decoder) = gen_client_server_instances(1, 4096);
        let request_headers = get_request_headers(false);
        insert_headers(&qpack_encoder, &qpack_decoder, request_headers);
        qpack_encoder.dump_dynamic_table();
        qpack_decoder.dump_dynamic_table();
    }

    #[test]
    fn insert_send_recv_refer_name_post() {
        let (qpack_encoder, qpack_decoder) = gen_client_server_instances(1, 4096);
        let request_headers = get_request_headers(false);
        insert_headers(&qpack_encoder, &qpack_decoder, request_headers);
        let mut request_headers = get_request_headers(true);
        request_headers = request_headers[..request_headers.len()/2-2].to_vec();

        let refer_dynamic_table = send_headers(&qpack_encoder, &qpack_decoder, request_headers);
        assert!(refer_dynamic_table);
    }

    fn insert_send_recv_many_prep(num: usize) -> Vec<Header> {
        let mut headers = vec![];
        headers.push(Header::from_str("", ""));
        let mut i = 0;
        loop {
            let header = &headers[i];
            let mut base_name = header.get_name().value.clone();
            let mut base_value = header.get_value().value.clone();

            for j in 0..26 {
                base_name.push(('a' as u8 + j) as char);
                base_value.push(('a' as u8 + j) as char);
                headers.push(Header::from_str(&base_name, &base_value));
                base_name.pop();
                base_value.pop();
            }
            if num <= headers.len() {
                break;
            }
            i += 1;
        }
        headers
    }

    #[test]
    fn insert_send_recv_many_at_once() {
        let num = 1024 * 20;
        let (qpack_encoder, qpack_decoder) = gen_client_server_instances(1, num * 2096);
        let headers = insert_send_recv_many_prep(num);
        insert_send_ack(&qpack_encoder, &qpack_decoder, headers, false);
    }

    #[test]
    fn insert_send_recv_many_one_by_one() {
        let num = 1024 * 20;
        let (qpack_encoder, qpack_decoder) = gen_client_server_instances(1, num * 2096);
        let mut headers = insert_send_recv_many_prep(num);

        let mut batch_size = 1;
        while 0 != headers.len() {
            let boundary = if batch_size <= headers.len() {batch_size} else {headers.len()};
            let request_headers = headers[..boundary].to_vec();
            headers = headers[boundary..].to_vec();
            insert_send_ack(&qpack_encoder, &qpack_decoder, request_headers, false);
            batch_size += 1;
        }
    }

    #[test]
    fn insert_send_recv() {
        let (qpack_client, qpack_server) = gen_client_server_instances(1, 4096);

        let request_headers = get_request_headers(false);
        insert_send_ack(&qpack_client, &qpack_server, request_headers, false);
    }

    #[test]
    fn insert_header_key_send_recv() {
        let (client, server) = gen_client_server_instances(1, 4096);
        let headers = get_request_headers(true);
        insert_headers(&client, &server, headers);
        let headers = get_request_headers(false);
        let refer_dynamic_table = send_headers(&client, &server, headers);
        assert!(refer_dynamic_table);
    }

    #[test]
    fn request_response() {
        let (qpack_client, qpack_server) = gen_client_server_instances(1, 1024);
        println!("Client -> Server");
        let request_headers = get_request_headers(false);
        insert_send_ack(&qpack_client, &qpack_server, request_headers, false);
        println!("Client <- Server");
        let response_headers = get_response_headers(false);
        insert_send_ack(&qpack_server, &qpack_client, response_headers, false);
    }

	#[test]
	fn rfc_appendix_b1_encode() {
		let qpack = Qpack::new(1, 1024);
		let headers = vec![Header::from_str(":path", "/index.html")];
		let mut encoded = vec![];
		let commit_func = qpack.encode_headers(&mut encoded, headers, STREAM_ID);
        commit(commit_func);
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
		assert_eq!(out.0, vec![Header::from_str(":path", "/index.html")]);
		assert_eq!(out.1, false);
	}

	#[test]
	fn encode_indexed_simple() {
		let qpack = Qpack::new(1, 1024);
		let headers = vec![Header::from_str(":path", "/")];
        let mut encoded = vec![];
		let commit_func = qpack.encode_headers(&mut encoded, headers, STREAM_ID);
        commit(commit_func);
		assert_eq!(encoded,
			vec![0x00, 0x00, 0xc1]);
	}
	#[test]
	fn decode_indexed_simple() {
		let qpack = Qpack::new(1, 1024);
		let wire = vec![0x00, 0x00, 0xc1];
		let out = qpack.decode_headers(&wire, STREAM_ID).unwrap();
		assert_eq!(out.0,
			vec![Header::from_str(":path", "/")]);
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
    fn blocking() {
        let (qpack_encoder, qpack_decoder) = gen_client_server_instances(1, 4096);
        let qpack_encoder = Arc::new(qpack_encoder);
        let qpack_decoder = Arc::new(qpack_decoder);
        let request_headers = get_request_headers(false);

        let mut insert_headers_packet = vec![];
        let commit_func = qpack_encoder.encode_insert_headers(&mut insert_headers_packet, request_headers.clone());
        commit(commit_func);

        // header insertion arrives after starting decoding headers
        let copied_dec = Arc::clone(&qpack_decoder);
        let th = thread::spawn(move || {
            let dur = time::Duration::from_secs(2);
            thread::sleep(dur);
            let commit_func = copied_dec.decode_encoder_instruction(&insert_headers_packet);
            commit(commit_func);
        });

        let refer_dynamic_table = send_headers(&qpack_encoder, &qpack_decoder, request_headers);
        if refer_dynamic_table {
            section_ackowledgment(&qpack_encoder, &qpack_decoder);
        }
        let _ = th.join();
    }

    #[test]
    fn multi_threading() {
        let (qpack_encoder, qpack_decoder) = gen_client_server_instances(2, 1024);
        let safe_encoder = Arc::new(qpack_encoder);
        let safe_decoder = Arc::new(qpack_decoder);

        let f = |headers: Vec<Header>, stream_id: u16, _expected_wire: Vec<u8>,
                                                encoder: Arc<Qpack>, decoder: Arc<Qpack>| {
            let mut encoded = vec![];
            let commit_func = encoder.encode_headers(&mut encoded, headers.clone(), stream_id);
            commit(commit_func);
            //assert_eq!(encoded, expected_wire);

            if let Ok(out) = decoder.decode_headers(&encoded, stream_id) {
                assert_eq!(out.0, headers);
                assert_eq!(out.1, false);
            } else {
                assert!(false);
            }
        };

        let mut ths = vec![];
        let headers_set = vec![vec![Header::from_str(":path", "/"), Header::from_str("age", "0")],
                                            vec![Header::from_str("content-length", "0"), Header::from_str(":method", "CONNECT")]];
        let expected_wires: Vec<Vec<u8>> = vec![vec![], vec![]];
        for i in 0..headers_set.len() {
            let en = Arc::clone(&safe_encoder);
            let de = Arc::clone(&safe_decoder);
            let headers = headers_set[i].clone();
            let expected_wire = expected_wires[i].clone();
            ths.push(thread::spawn(move || {
                f(headers, 4 + (i as u16) * 2, expected_wire, en, de);
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
            commit(commit_func);
            let headers = vec![Header::from_str(":authority", "www.example.com"),
                                          Header::from_str(":path", "/sample/path")];

            let commit_func = qpack_encoder.encode_insert_headers(&mut encoded, headers);
            assert_eq!(encoded, vec![0x3f, 0xbd, 0x01, 0xc0, 0x0f, 0x77, 0x77,
                                    0x77, 0x2e, 0x65, 0x78, 0x61, 0x6d, 0x70,
                                    0x6c, 0x65, 0x2e, 0x63, 0x6f, 0x6d, 0xc1,
                                    0x0c, 0x2f, 0x73, 0x61, 0x6d, 0x70, 0x6c,
                                    0x65, 0x2f, 0x70, 0x61, 0x74, 0x68]);
            commit(commit_func);

            let commit_func = qpack_decoder.decode_encoder_instruction(&encoded);
            commit(commit_func);

            qpack_encoder.dump_dynamic_table();
            qpack_decoder.dump_dynamic_table();
        }

        println!("Step 2");
        {   // header transfer
            let mut encoded = vec![];
            let headers = vec![Header::from_str(":authority", "www.example.com"),
                                          Header::from_str(":path", "/sample/path")];
            let commit_func = qpack_encoder.encode_headers(&mut encoded, headers.clone(), STREAM_ID);
            commit(commit_func);
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
            commit(commit_func);

            let commit_func = qpack_encoder.decode_decoder_instruction(&encoded);
            commit(commit_func);
            println!("dump encoder side dynamic table");
            qpack_encoder.dump_dynamic_table();
            println!("dump decoder side dynamic table");
            qpack_decoder.dump_dynamic_table();
        }

        println!("Step 4");
        {   // encoder instruction
            let mut encoded = vec![];
            let headers = vec![Header::from_str("custom-key", "custom-value")];
            let commit_func = qpack_encoder.encode_insert_headers(&mut encoded, headers);
            assert_eq!(encoded, vec![0x4a, 0x63, 0x75, 0x73, 0x74, 0x6f, 0x6d, 0x2d, 0x6b, 0x65,
                                    0x79, 0x0c, 0x63, 0x75, 0x73, 0x74, 0x6f, 0x6d, 0x2d, 0x76,
                                    0x61, 0x6c, 0x75, 0x65]);
            commit(commit_func);

            let commit_func = qpack_decoder.decode_encoder_instruction(&encoded);
            commit(commit_func);

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
            commit(commit_func);

            let commit_func = qpack_encoder.decode_decoder_instruction(&encoded);
            commit(commit_func);

            qpack_encoder.dump_dynamic_table();
            qpack_decoder.dump_dynamic_table();
        }

        println!("Step 6");
        {   // encoder instruction
            let mut encoded = vec![];
            let headers = vec![Header::from_str(":authority", "www.example.com")];
            let commit_func = qpack_encoder.encode_insert_headers(&mut encoded, headers);
            assert_eq!(encoded, vec![0x02]);
            commit(commit_func);

            let commit_func = qpack_decoder.decode_encoder_instruction(&encoded);
            commit(commit_func);

            qpack_encoder.dump_dynamic_table();
            qpack_decoder.dump_dynamic_table();
        }

        println!("Step 7");
        {   // header transfer
            let mut encoded = vec![];
            let headers = vec![Header::from_str(":authority", "www.example.com"),
                                        Header::from_str(":path", "/"),
                                        Header::from_str("custom-key", "custom-value")];
            let commit_func = qpack_encoder.encode_headers(&mut encoded, headers.clone(), 8);
            commit(commit_func);
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
            commit(commit_func);

            let commit_func = qpack_encoder.decode_decoder_instruction(&encoded);
            commit(commit_func);
        }

        println!("Step 9");
        {   // encoder instruction
            let mut encoded = vec![];
            let headers = vec![Header::from_str("custom-key", "custom-value2")];
            let commit_func = qpack_encoder.encode_insert_headers(&mut encoded, headers);
            assert_eq!(encoded, vec![0x81, 0x0d, 0x63, 0x75, 0x73, 0x74, 0x6f, 0x6d,
                                     0x2d, 0x76, 0x61, 0x6c, 0x75, 0x65, 0x32]);
            commit(commit_func);

            let commit_func = qpack_decoder.decode_encoder_instruction(&encoded);
            commit(commit_func);

            println!("dump encoder side dynamic table");
            qpack_encoder.dump_dynamic_table();
            println!("dump decoder side dynamic table");
            qpack_decoder.dump_dynamic_table();
        }
    }
}