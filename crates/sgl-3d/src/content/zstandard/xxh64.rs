//! XXH64 (seed 0 in zstd), streaming, used for the optional frame content checksum.
const P1: u64 = 0x9E37_79B1_85EB_CA87;
const P2: u64 = 0xC2B2_AE3D_27D4_EB4F;
const P3: u64 = 0x1656_67B1_9E37_79F9;
const P4: u64 = 0x85EB_CA77_C2B2_AE63;
const P5: u64 = 0x27D4_EB2F_1656_67C5;

#[inline(always)]
fn round(acc: u64, lane: u64) -> u64 {
    acc.wrapping_add(lane.wrapping_mul(P2))
        .rotate_left(31)
        .wrapping_mul(P1)
}

#[inline(always)]
fn merge(acc: u64, v: u64) -> u64 {
    (acc ^ round(0, v)).wrapping_mul(P1).wrapping_add(P4)
}

#[inline(always)]
fn le64(b: &[u8]) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[..8]);
    u64::from_le_bytes(a)
}

/// Incremental XXH64 state.
#[derive(Clone)]
pub struct Xxh64 {
    v: [u64; 4],
    buf: [u8; 32],
    buf_len: usize,
    total: u64,
    seed: u64,
}

impl Xxh64 {
    /// Starts a hash with the given seed (zstd uses 0).
    pub fn new(seed: u64) -> Self {
        Xxh64 {
            v: [
                seed.wrapping_add(P1).wrapping_add(P2),
                seed.wrapping_add(P2),
                seed,
                seed.wrapping_sub(P1),
            ],
            buf: [0; 32],
            buf_len: 0,
            total: 0,
            seed,
        }
    }

    fn stripe(v: &mut [u64; 4], s: &[u8]) {
        v[0] = round(v[0], le64(&s[0..]));
        v[1] = round(v[1], le64(&s[8..]));
        v[2] = round(v[2], le64(&s[16..]));
        v[3] = round(v[3], le64(&s[24..]));
    }

    /// Feeds more bytes.
    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        if self.buf_len > 0 {
            let take = (32 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len < 32 {
                return;
            }
            let b = self.buf;
            Self::stripe(&mut self.v, &b);
            self.buf_len = 0;
        }
        let mut chunks = data.chunks_exact(32);
        for c in &mut chunks {
            Self::stripe(&mut self.v, c);
        }
        let rest = chunks.remainder();
        self.buf[..rest.len()].copy_from_slice(rest);
        self.buf_len = rest.len();
    }

    /// Returns the 64-bit digest (the state is not consumed).
    pub fn digest(&self) -> u64 {
        let mut h = if self.total >= 32 {
            let v = self.v;
            let mut h = v[0]
                .rotate_left(1)
                .wrapping_add(v[1].rotate_left(7))
                .wrapping_add(v[2].rotate_left(12))
                .wrapping_add(v[3].rotate_left(18));
            for x in v {
                h = merge(h, x);
            }
            h
        } else {
            self.seed.wrapping_add(P5)
        };
        h = h.wrapping_add(self.total);
        let mut rest = &self.buf[..self.buf_len];
        while rest.len() >= 8 {
            h ^= round(0, le64(rest));
            h = h.rotate_left(27).wrapping_mul(P1).wrapping_add(P4);
            rest = &rest[8..];
        }
        if rest.len() >= 4 {
            let w = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]) as u64;
            h ^= w.wrapping_mul(P1);
            h = h.rotate_left(23).wrapping_mul(P2).wrapping_add(P3);
            rest = &rest[4..];
        }
        for &b in rest {
            h ^= (b as u64).wrapping_mul(P5);
            h = h.rotate_left(11).wrapping_mul(P1);
        }
        h ^= h >> 33;
        h = h.wrapping_mul(P2);
        h ^= h >> 29;
        h = h.wrapping_mul(P3);
        h ^ (h >> 32)
    }
}

/// One-shot XXH64.
pub fn xxh64(data: &[u8], seed: u64) -> u64 {
    let mut h = Xxh64::new(seed);
    h.update(data);
    h.digest()
}
