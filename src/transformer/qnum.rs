pub struct Qnum;
impl Qnum {
    pub fn encode(encoded: &mut Vec<u8>, val: u32, n: u8) -> usize {
		let mut val = val;
        let mut len = 1;
        let mask: u8 = if n == 8 {
            0xff
        } else {
            (1 << n) - 1
        };
        if val < mask as u32 {
            encoded.push(val as u8);
            return len;
        }

        encoded.push(mask);
        val -= mask as u32;
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


#[cfg(test)]
mod tests {
    use crate::transformer::qnum::Qnum;
    #[test]
    fn encode_decode() {
        let mut values: Vec<u32> = (0..(u16::MAX as u32 * 2)).collect();
        values.push(u32::MAX);
        values.push(u32::MAX-1);

        for i in values {
            for j in 1..=8 {
                let mut encoded = vec![];
                let len = Qnum::encode(&mut encoded, i, j);
                let out = Qnum::decode(&encoded, 0, j);
                assert_eq!(i, out.1);
                assert_eq!(len, out.0);
            }
        }
    }
}
