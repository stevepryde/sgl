//! Internal dictionary state retained by the upstream decoder. SGL supplies no dictionary.
#[derive(Clone, Debug)]
pub struct Dictionary {
    pub(crate) id: u32,
    pub(crate) content: Vec<u8>,
    pub(crate) reps: [u32; 3],
}
