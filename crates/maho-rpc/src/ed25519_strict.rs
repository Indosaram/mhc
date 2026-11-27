//! Port of senpi `packages/coding-agent/src/modes/rpc/ed25519-strict.ts`.
//!
//! Strict Ed25519 verification for sealed secret transcripts (D11 secret phase; omo-relay-spec
//! decisions 0003 and 0022). Before the native equation runs, refuses: a public key or R that is
//! not the canonical encoding of a curve point, either one with order dividing 8, a public key
//! outside the prime-order subgroup, and S >= L.

use ed25519_dalek::{Signature, VerifyingKey};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictEd25519Refusal {
    Length,
    Encoding,
    SmallOrder,
    NotPrimeOrder,
    Scalar,
}

/// 256-bit unsigned integer in little-endian 64-bit limbs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct U256([u64; 4]);

impl U256 {
    const ZERO: Self = Self([0, 0, 0, 0]);
    const ONE: Self = Self([1, 0, 0, 0]);

    const fn from_le_bytes(bytes: &[u8]) -> Self {
        let mut limbs = [0u64; 4];
        let mut i = 0;
        while i < 32 && i < bytes.len() {
            limbs[i / 8] |= (bytes[i] as u64) << ((i % 8) * 8);
            i += 1;
        }
        Self(limbs)
    }

    const fn is_odd(&self) -> bool {
        (self.0[0] & 1) == 1
    }

    const fn bit(&self, idx: usize) -> bool {
        let limb = idx / 64;
        let shift = idx % 64;
        if limb >= 4 {
            false
        } else {
            ((self.0[limb] >> shift) & 1) == 1
        }
    }

    const fn add_with_carry(self, rhs: Self) -> (Self, bool) {
        let mut out = [0u64; 4];
        let mut carry = 0u128;
        let mut i = 0;
        while i < 4 {
            let sum = self.0[i] as u128 + rhs.0[i] as u128 + carry;
            out[i] = sum as u64;
            carry = sum >> 64;
            i += 1;
        }
        (Self(out), carry != 0)
    }

    const fn sub_unchecked(self, rhs: Self) -> Self {
        let mut out = [0u64; 4];
        let mut borrow = 0i128;
        let mut i = 0;
        while i < 4 {
            let diff = self.0[i] as i128 - rhs.0[i] as i128 - borrow;
            if diff < 0 {
                out[i] = (diff + (1i128 << 64)) as u64;
                borrow = 1;
            } else {
                out[i] = diff as u64;
                borrow = 0;
            }
            i += 1;
        }
        Self(out)
    }

    const fn ge(self, rhs: Self) -> bool {
        let mut i = 4;
        while i > 0 {
            i -= 1;
            if self.0[i] > rhs.0[i] {
                return true;
            }
            if self.0[i] < rhs.0[i] {
                return false;
            }
        }
        true
    }
}

/// P = 2^255 - 19
const P: U256 = U256([u64::MAX - 18, u64::MAX, u64::MAX, (1u64 << 63) - 1]);
/// P - 2 = 2^255 - 21
const P_MINUS_2: U256 = U256([u64::MAX - 20, u64::MAX, u64::MAX, (1u64 << 63) - 1]);
/// (P - 1) / 4 = 2^253 - 5
const P_MINUS_1_DIV_4: U256 = U256([u64::MAX - 4, u64::MAX, u64::MAX, (1u64 << 61) - 1]);
/// (P - 5) / 8 = 2^252 - 3
const P_MINUS_5_DIV_8: U256 = U256([u64::MAX - 2, u64::MAX, u64::MAX, (1u64 << 60) - 1]);

/// L = 2^252 + 27742317777372353535851937790883648493
const fn ed25519_order() -> U256 {
    let low: u128 = 27_742_317_777_372_353_535_851_937_790_883_648_493;
    U256([low as u64, (low >> 64) as u64, 0, 1u64 << 60])
}
const L: U256 = ed25519_order();

const fn mod_add(a: U256, b: U256) -> U256 {
    let (sum, carry) = a.add_with_carry(b);
    if carry || sum.ge(P) {
        sum.sub_unchecked(P)
    } else {
        sum
    }
}

const fn mod_neg(a: U256) -> U256 {
    if a.0[0] == 0 && a.0[1] == 0 && a.0[2] == 0 && a.0[3] == 0 {
        U256::ZERO
    } else {
        P.sub_unchecked(a)
    }
}

const fn mod_sub(a: U256, b: U256) -> U256 {
    if a.ge(b) {
        a.sub_unchecked(b)
    } else {
        P.sub_unchecked(b.sub_unchecked(a))
    }
}

const fn mod_mul(a: U256, b: U256) -> U256 {
    let mut prod = [0u64; 8];
    let mut i = 0;
    while i < 4 {
        let mut carry = 0u128;
        let mut j = 0;
        while j < 4 {
            let v = (a.0[i] as u128) * (b.0[j] as u128) + (prod[i + j] as u128) + carry;
            prod[i + j] = v as u64;
            carry = v >> 64;
            j += 1;
        }
        prod[i + 4] = carry as u64;
        i += 1;
    }
    // Fold upper 256 bits with 2^256 ≡ 38 (mod P).
    let mut reduced = [0u64; 5];
    let mut carry = 0u128;
    let mut k = 0;
    while k < 4 {
        let v = (prod[k] as u128) + 38u128 * (prod[k + 4] as u128) + carry;
        reduced[k] = v as u64;
        carry = v >> 64;
        k += 1;
    }
    reduced[4] = carry as u64;
    // Fold bits above 255 with 2^255 ≡ 19 (mod P).
    let top = ((reduced[4] as u128) << 1) | ((reduced[3] >> 63) as u128);
    reduced[3] &= (1u64 << 63) - 1;
    let mut c2 = top * 19u128;
    let mut out = [0u64; 4];
    let mut m = 0;
    while m < 4 {
        let v = (reduced[m] as u128) + c2;
        out[m] = v as u64;
        c2 = v >> 64;
        m += 1;
    }
    // At most one extra bit above 255 can remain.
    let top2 = (c2 << 1) | ((out[3] >> 63) as u128);
    out[3] &= (1u64 << 63) - 1;
    let mut c3 = top2 * 19u128;
    let mut n = 0;
    while n < 4 {
        let v = (out[n] as u128) + c3;
        out[n] = v as u64;
        c3 = v >> 64;
        n += 1;
    }
    let res = U256(out);
    if res.ge(P) { res.sub_unchecked(P) } else { res }
}

const fn mod_pow(base: U256, exp: U256) -> U256 {
    let mut result = U256::ONE;
    let mut b = if base.ge(P) {
        base.sub_unchecked(P)
    } else {
        base
    };
    let mut bit = 0;
    while bit < 256 {
        if exp.bit(bit) {
            result = mod_mul(result, b);
        }
        b = mod_mul(b, b);
        bit += 1;
    }
    result
}

const fn mod_from_u64(v: u64) -> U256 {
    U256([v, 0, 0, 0])
}

const D: U256 = mod_mul(
    mod_neg(mod_from_u64(121665)),
    mod_pow(mod_from_u64(121666), P_MINUS_2),
);
const SQRT_MINUS_ONE: U256 = mod_pow(mod_from_u64(2), P_MINUS_1_DIV_4);

#[derive(Clone, Copy)]
struct Point {
    x: U256,
    y: U256,
    z: U256,
    t: U256,
}

const IDENTITY: Point = Point {
    x: U256::ZERO,
    y: U256::ONE,
    z: U256::ONE,
    t: U256::ZERO,
};

fn point_add(p: Point, q: Point) -> Point {
    let a = mod_mul(mod_sub(p.y, p.x), mod_sub(q.y, q.x));
    let b = mod_mul(mod_add(p.y, p.x), mod_add(q.y, q.x));
    let c = mod_mul(mod_mul(mod_from_u64(2), D), mod_mul(p.t, q.t));
    let d = mod_mul(mod_from_u64(2), mod_mul(p.z, q.z));
    let e = mod_sub(b, a);
    let f = mod_sub(d, c);
    let g = mod_add(d, c);
    let h = mod_add(b, a);
    Point {
        x: mod_mul(e, f),
        y: mod_mul(g, h),
        z: mod_mul(f, g),
        t: mod_mul(e, h),
    }
}

fn point_times(point: Point, scalar: U256) -> Point {
    let mut result = IDENTITY;
    let mut addend = point;
    for bit in 0..256 {
        if scalar.bit(bit) {
            result = point_add(result, addend);
        }
        addend = point_add(addend, addend);
    }
    result
}

fn is_identity(p: Point) -> bool {
    p.x == U256::ZERO && mod_sub(p.y, p.z) == U256::ZERO
}

fn decode_point(bytes: &[u8]) -> Option<Point> {
    if bytes.len() != 32 {
        return None;
    }
    let sign = (bytes[31] >> 7) & 1;
    let mut y = U256::from_le_bytes(bytes);
    y.0[3] &= (1u64 << 63) - 1;
    if y.ge(P) {
        return None;
    }
    let y2 = mod_mul(y, y);
    let u = mod_sub(y2, U256::ONE);
    let v = mod_add(mod_mul(D, y2), U256::ONE);
    let v3 = mod_mul(mod_mul(v, v), v);
    let v7 = mod_mul(mod_mul(v3, v3), v);
    let mut x = mod_mul(mod_mul(u, v3), mod_pow(mod_mul(u, v7), P_MINUS_5_DIV_8));
    let vx2 = mod_mul(v, mod_mul(x, x));
    if vx2 == mod_neg(u) {
        x = mod_mul(x, SQRT_MINUS_ONE);
    } else if vx2 != u {
        return None;
    }
    if x == U256::ZERO && sign == 1 {
        return None;
    }
    if u8::from(x.is_odd()) != sign {
        x = mod_neg(x);
    }
    Some(Point {
        x,
        y,
        z: U256::ONE,
        t: mod_mul(x, y),
    })
}

fn small_order(p: Point) -> bool {
    is_identity(point_times(p, mod_from_u64(8)))
}

/// The strict prechecks alone, run before the native equation; `None` when all pass.
pub fn strict_ed25519_refusal(public_key: &[u8], signature: &[u8]) -> Option<StrictEd25519Refusal> {
    if public_key.len() != 32 || signature.len() != 64 {
        return Some(StrictEd25519Refusal::Length);
    }
    let a = decode_point(public_key)?;
    let r = decode_point(&signature[..32])?;
    if small_order(a) || small_order(r) {
        return Some(StrictEd25519Refusal::SmallOrder);
    }
    if !is_identity(point_times(a, L)) {
        return Some(StrictEd25519Refusal::NotPrimeOrder);
    }
    if U256::from_le_bytes(&signature[32..]).ge(L) {
        return Some(StrictEd25519Refusal::Scalar);
    }
    None
}

// Preserve distinct `Encoding` refusal when `decode_point` returns `None`.
fn strict_ed25519_refusal_inner(
    public_key: &[u8],
    signature: &[u8],
) -> Option<StrictEd25519Refusal> {
    if public_key.len() != 32 || signature.len() != 64 {
        return Some(StrictEd25519Refusal::Length);
    }
    let Some(a) = decode_point(public_key) else {
        return Some(StrictEd25519Refusal::Encoding);
    };
    let Some(r) = decode_point(&signature[..32]) else {
        return Some(StrictEd25519Refusal::Encoding);
    };
    if small_order(a) || small_order(r) {
        return Some(StrictEd25519Refusal::SmallOrder);
    }
    if !is_identity(point_times(a, L)) {
        return Some(StrictEd25519Refusal::NotPrimeOrder);
    }
    if U256::from_le_bytes(&signature[32..]).ge(L) {
        return Some(StrictEd25519Refusal::Scalar);
    }
    None
}

/// Strict prechecks alone, matching TS `strictEd25519Refusal`.
pub fn strict_ed25519_check(public_key: &[u8], signature: &[u8]) -> Option<StrictEd25519Refusal> {
    strict_ed25519_refusal_inner(public_key, signature)
}

/// Verifies `signature` (64 bytes) over `message` under the raw 32-byte public key with the
/// strict rules above. Returns `false` for every refusal.
pub fn strict_ed25519_verify(public_key: &[u8], message: &[u8], signature: &[u8]) -> bool {
    if strict_ed25519_refusal_inner(public_key, signature).is_some() {
        return false;
    }
    let Ok(pk_bytes): Result<[u8; 32], _> = public_key.try_into() else {
        return false;
    };
    let Ok(vk) = VerifyingKey::from_bytes(&pk_bytes) else {
        return false;
    };
    let Ok(sig) = Signature::from_slice(signature) else {
        return false;
    };
    vk.verify_strict(message, &sig).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn valid_signature_passes_and_invalid_inputs_refuse_with_specific_reasons() {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let vk = sk.verifying_key();
        let msg = b"omo-secret-answer-v1\n[\"req\"]";
        let sig = sk.sign(msg).to_bytes();

        assert_eq!(strict_ed25519_check(vk.as_bytes(), &sig), None);
        assert!(strict_ed25519_verify(vk.as_bytes(), msg, &sig));
        assert!(!strict_ed25519_verify(vk.as_bytes(), b"other", &sig));

        assert_eq!(
            strict_ed25519_check(&[0u8; 31], &sig),
            Some(StrictEd25519Refusal::Length)
        );

        // Identity point [1, 0, ...] is order 1 (divides 8).
        let mut identity = [0u8; 32];
        identity[0] = 1;
        assert_eq!(
            strict_ed25519_check(&identity, &sig),
            Some(StrictEd25519Refusal::SmallOrder)
        );
        assert!(!strict_ed25519_verify(&identity, msg, &sig));

        // y = P is non-canonical encoding.
        let non_canonical = [
            0xed, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0x7f,
        ];
        assert_eq!(
            strict_ed25519_check(&non_canonical, &sig),
            Some(StrictEd25519Refusal::Encoding)
        );

        // S = L >= L is refused as Scalar.
        let mut bad_s = sig;
        for (i, limb) in L.0.iter().enumerate() {
            bad_s[32 + i * 8..32 + (i + 1) * 8].copy_from_slice(&limb.to_le_bytes());
        }
        assert_eq!(
            strict_ed25519_check(vk.as_bytes(), &bad_s),
            Some(StrictEd25519Refusal::Scalar)
        );
        assert!(!strict_ed25519_verify(vk.as_bytes(), msg, &bad_s));
    }
}
