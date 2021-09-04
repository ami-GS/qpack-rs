// StrHeader will be implemented later once all works
// I assume &str header's would be slow due to page fault
pub type StrHeader<'a> = (&'a str, &'a str);
#[derive(PartialEq, Eq, Debug, Clone)]
pub struct Header(pub String, pub String);

impl Header {
    pub fn from_str(name: &str, value: &str) -> Self {
        Self(String::from(name), String::from(value))
    }
    pub fn from_string(name: String, value: String) -> Self {
        Self(name, value)
    }
    pub fn from_str_header(header: StrHeader) -> Self {
        Self(header.0.to_string(), header.1.to_string())
    }
    pub fn size(&self) -> usize {
        self.0.len() + self.1.len() + 32
    }
}

impl From<DynamicHeader> for Header {
    fn from(header: DynamicHeader) -> Self {
        Self(*header.0, header.1)
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
        Self(Box::new(header.0), header.1)
    }
}
