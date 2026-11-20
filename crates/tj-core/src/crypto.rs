//! SHA-256, HMAC-SHA256 and HMAC-DRBG (NIST SP 800-90A), for `crypto.*` and for ids that must
//! not be guessable. Written out here so device builds need no crate for them.
//!
//! The generator starts from the host's boot seed and the time, which only make it unique;
//! it becomes unguessable once the host sends 32 secure random bytes (`entropy` input).
//! Each delivery is mixed in and never replaces what is there, so a host or automation
//! client that sends known bytes cannot make the output predictable.

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&(data.len() as u64).wrapping_mul(8).to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..64 {
            w[i] = if i < 16 {
                u32::from_be_bytes([block[4 * i], block[4 * i + 1], block[4 * i + 2], block[4 * i + 3]])
            } else {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1)
            };
        }
        let mut s = h;
        for i in 0..64 {
            let [a, b, c, d, e, f, g, hh] = s;
            let t1 = hh.wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25)).wrapping_add((e & f) ^ (!e & g)).wrapping_add(K[i]).wrapping_add(w[i]);
            let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22)).wrapping_add((a & b) ^ (a & c) ^ (b & c));
            s = [t1.wrapping_add(t2), a, b, c, d.wrapping_add(t1), e, f, g];
        }
        for (x, y) in h.iter_mut().zip(s) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 32];
    for (i, x) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

/// HMAC-SHA256 of the concatenated `parts`.
pub fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&sha256(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    for p in parts {
        inner.extend_from_slice(p);
    }
    let mut outer: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    outer.extend_from_slice(&sha256(&inner));
    sha256(&outer)
}

/// Compares in constant time for equal lengths, so the time taken does not reveal how much
/// matched. Lengths are not secret.
pub fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// HMAC-DRBG with SHA-256.
pub struct Drbg {
    k: [u8; 32],
    v: [u8; 32],
}

impl Drbg {
    pub fn new(seed: &[u8]) -> Drbg {
        let mut d = Drbg { k: [0; 32], v: [1; 32] };
        d.update(seed);
        d
    }

    /// Mixes `entropy` into the state; what was there is kept.
    pub fn reseed(&mut self, entropy: &[u8]) {
        self.update(entropy);
    }

    fn update(&mut self, data: &[u8]) {
        self.k = hmac(&self.k, &[&self.v, &[0], data]);
        self.v = hmac(&self.k, &[&self.v]);
        if !data.is_empty() {
            self.k = hmac(&self.k, &[&self.v, &[1], data]);
            self.v = hmac(&self.k, &[&self.v]);
        }
    }

    pub fn fill(&mut self, out: &mut [u8]) {
        for chunk in out.chunks_mut(32) {
            self.v = hmac(&self.k, &[&self.v]);
            chunk.copy_from_slice(&self.v[..chunk.len()]);
        }
        self.update(&[]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_and_hmac_match_the_published_vectors() {
        assert_eq!(hex(&sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(hex(&sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")), "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1");
        assert_eq!(hex(&sha256(&[b'a'; 1_000_000])), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
        // RFC 4231, cases 2 and 6 (a key longer than a block).
        assert_eq!(hex(&hmac(b"Jefe", &[b"what do ya want ", b"for nothing?"])), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        assert_eq!(hex(&hmac(&[0xaa; 131], &[b"Test Using Larger Than Block-Size Key - Hash Key First"])), "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54");
        assert!(same(b"abc", b"abc") && !same(b"abc", b"abd") && !same(b"abc", b"ab"));
    }

    #[test]
    fn the_generator_is_repeatable_and_entropy_only_adds() {
        let draw = |d: &mut Drbg| {
            let mut out = [0u8; 40];
            d.fill(&mut out);
            out
        };
        let (mut a, mut b) = (Drbg::new(b"seed"), Drbg::new(b"seed"));
        assert_eq!(draw(&mut a), draw(&mut b), "the same seed, the same bytes");
        assert_ne!(draw(&mut a), draw(&mut a), "never the same twice");
        // Known bytes mixed into different states still give different output.
        let mut c = Drbg::new(b"another seed");
        a.reseed(&[0; 32]);
        c.reseed(&[0; 32]);
        assert_ne!(draw(&mut a), draw(&mut c));
    }
}
