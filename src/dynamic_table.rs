pub struct DynamicTable {
    // #1.1 The total number of entries inserted in the dynamic table
    pub insert_count: u16,
    pub max_capacity: u32,
}

impl DynamicTable {
    pub fn new() -> Self {
        Self {
            insert_count: 0,
            max_capacity: 0,
        }
    }
    // fn add() -> Result<(), Box<dyn error::Error>> { // TODO: QPACK_ENCODER_STREAM_ERROR
    //     Err(EncoderStreamError.into())
    // }
    // fn evict() {
    // }
    // fn set_capacity() -> Result<(), Box<dyn error::Error>> {
    //     // error when to set 0. see $3.2.3
    //     // error when exceed limit as QPACK_ENCODER_STREAM_ERROR?
    //     Err(EncoderStreamError.into())
    // }
}
