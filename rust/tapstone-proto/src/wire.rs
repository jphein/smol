//! Little-endian cursor helpers. Every read is bounds-checked and returns `None` past the end.

pub(crate) struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Reader { b, i: 0 }
    }
    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.i..self.i.checked_add(n)?)?;
        self.i += n;
        Some(s)
    }
    pub fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Option<u16> {
        let s = self.take(2)?;
        Some(u16::from_le_bytes([s[0], s[1]]))
    }
    pub fn u32(&mut self) -> Option<u32> {
        let s = self.take(4)?;
        Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    pub fn u64(&mut self) -> Option<u64> {
        let mut a = [0u8; 8];
        a.copy_from_slice(self.take(8)?);
        Some(u64::from_le_bytes(a))
    }
    pub fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        let mut a = [0u8; N];
        a.copy_from_slice(self.take(N)?);
        Some(a)
    }
}

pub(crate) struct Writer<'a> {
    b: &'a mut [u8],
    i: usize,
}

impl<'a> Writer<'a> {
    pub fn new(b: &'a mut [u8]) -> Self {
        Writer { b, i: 0 }
    }
    pub fn len(&self) -> usize {
        self.i
    }
    /// Panics if the buffer is too small: callers pass `[u8; FRAME_MAX]`, which every frame fits
    /// by construction (asserted in tests), so running out is a codec bug, not an input.
    pub fn bytes(&mut self, s: &[u8]) {
        self.b[self.i..self.i + s.len()].copy_from_slice(s);
        self.i += s.len();
    }
    pub fn u8(&mut self, v: u8) {
        self.bytes(&[v]);
    }
    pub fn u16(&mut self, v: u16) {
        self.bytes(&v.to_le_bytes());
    }
    pub fn u32(&mut self, v: u32) {
        self.bytes(&v.to_le_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }
}
