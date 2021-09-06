use std::error;

// StrHeader will be implemented later once all works
// I assume &str header's would be slow due to page fault
pub type StrHeader<'a> = (&'a str, &'a str);
#[derive(PartialEq, Eq, Debug, Clone)]
pub struct HeaderString {
    pub value: String,
    pub huffman: bool,
}
impl HeaderString {
    pub fn new(value: String, huffman: bool) -> Self {
        Self {value, huffman}
    }
}

#[derive(PartialEq, Eq, Debug, Clone)]
pub struct Header {
    name: HeaderString,
    value: HeaderString,
    pub sensitive: bool
}

impl Header {
    pub fn new(name: String, value: String, sensitive: bool) -> Self {
        Self {
            name: HeaderString::new(name, false),
            value: HeaderString::new(value, false),
            sensitive,
        }
    }
    pub fn new_huffman(name: String, value: String, sensitive: bool) -> Self {
        Self {
            name: HeaderString::new(name, true),
            value: HeaderString::new(value, true),
            sensitive,
        }
    }
    pub fn new_with_header_string(name: HeaderString, value: HeaderString, sensitive: bool) -> Self {
        Self {
            name,
            value,
            sensitive,
        }
    }
    pub fn from_str(name: &str, value: &str) -> Self {
        Self {
            name: HeaderString::new(name.to_string(), false),
            value: HeaderString::new(value.to_string(), false),
            sensitive: false,
        }
    }
    pub fn from_string(name: String, value: String) -> Self {
        // from_string is called by decoding process. flags should not be needed
        Self {
            name: HeaderString::new(name, false),
            value: HeaderString::new(value, false),
            sensitive: false,
        }
    }
    pub fn size(&self) -> usize {
        self.name.value.len() + self.value.value.len() + 32
    }
    pub fn get_name(&self) -> &HeaderString {
        &self.name
    }
    pub fn get_value(&self) -> &HeaderString {
        &self.value
    }
    pub fn move_value(self) -> HeaderString {
        self.value
    }
    pub fn set_value(&mut self, value: HeaderString) {
        self.value = value;
    }
    pub fn set_sensitive(&mut self, sensitive: bool) {
        self.sensitive = sensitive;
    }
}

impl From<StrHeader<'_>> for Header {
    fn from(header: StrHeader) -> Self {
        Self {
            name: HeaderString::new(header.0.to_string(), false),
            value: HeaderString::new(header.1.to_string(), false),
            sensitive: false,
        }
    }
}

impl From<DynamicHeader> for Header {
    fn from(header: DynamicHeader) -> Self {
        Header::from_string(*header.0, header.1)
    }
}

// TODO: trait for Header and DynamicHeader
#[derive(PartialEq, Eq, Debug, Clone)]
pub struct DynamicHeader(pub Box<String>, pub String);
impl DynamicHeader {
    pub fn from_str(name: &str, value: &str) -> Self {
        Self(Box::new(name.to_owned()), value.to_owned())
    }
    pub fn size(&self) -> usize {
        self.0.len() + self.1.len() + 32
    }
}

impl From<Header> for DynamicHeader {
    fn from(header: Header) -> Self {
        Self(Box::new(header.name.value), header.value.value)
    }
}

pub type CommitFunc = Box<dyn FnOnce() -> Result<(), Box<dyn error::Error>>>;