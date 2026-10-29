//! Strict Ed25519/X25519 admission checks (decision 0003, prime-key amendment), ported from the
//! vendored `strict-ed25519.ts`: canonical on-curve A/R, no negative zero or order-dividing-8
//! points; public KEYS additionally require [L]A = identity (1024-entry byte-keyed LRU). S < L; the
//! native verifier computes the equation ONLY after these prechecks. Variable-time group
//! arithmetic handles public values only.
use aws_lc_rs::signature::{ED25519, UnparsedPublicKey};
use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictEd25519Refusal {
    Malformed,
    Signature,
}
impl StrictEd25519Refusal {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::Signature => "signature",
        }
    }
}
pub type StrictEd25519Verdict = Result<(), StrictEd25519Refusal>;

type Fe = [u64; 4];
const P: Fe = [
    0xffff_ffff_ffff_ffed,
    0xffff_ffff_ffff_ffff,
    0xffff_ffff_ffff_ffff,
    0x7fff_ffff_ffff_ffff,
];
/// L = 2^252 + 27742317777372353535851937790883648493
const L: [u64; 4] = [
    0x5812_631a_5cf5_d3ed,
    0x14de_f9de_a2f7_9cd6,
    0,
    0x1000_0000_0000_0000,
];
/// P - 2
const P_MINUS_2: Fe = [
    0xffff_ffff_ffff_ffeb,
    0xffff_ffff_ffff_ffff,
    0xffff_ffff_ffff_ffff,
    0x7fff_ffff_ffff_ffff,
];
/// (P - 1) / 4 = 2^253 - 5
const P_MINUS_1_OVER_4: Fe = [
    0xffff_ffff_ffff_fffb,
    0xffff_ffff_ffff_ffff,
    0xffff_ffff_ffff_ffff,
    0x1fff_ffff_ffff_ffff,
];
/// (P - 5) / 8 = 2^252 - 3
const SQRT_EXP: Fe = [
    0xffff_ffff_ffff_fffd,
    0xffff_ffff_ffff_ffff,
    0xffff_ffff_ffff_ffff,
    0x0fff_ffff_ffff_ffff,
];
const X25519_ORDER8: [Fe; 2] = [
    [
        0xaeb8_413b_7c7a_ebe0,
        0x6ac4_9ff1_fae3_5616,
        0xfdb1_329c_eb8d_09da,
        0x00b8_495f_1605_6286,
    ],
    [
        0x248c_50a3_bc95_9c5f,
        0x5bef_839c_55b1_d0b1,
        0x868e_1c58_c45c_4404,
        0x5711_9fd0_dd4e_22d8,
    ],
];
const ZERO: Fe = [0; 4];
const ONE: Fe = [1, 0, 0, 0];

fn ge_p(a: &Fe) -> bool {
    for i in (0..4).rev() {
        if a[i] != P[i] {
            return a[i] > P[i];
        }
    }
    true
}
fn sub_raw(a: &Fe, b: &Fe) -> (Fe, bool) {
    let mut out = ZERO;
    let mut borrow = false;
    for i in 0..4 {
        let (v, b1) = a[i].overflowing_sub(b[i]);
        let (v, b2) = v.overflowing_sub(u64::from(borrow));
        out[i] = v;
        borrow = b1 || b2;
    }
    (out, borrow)
}
fn add_raw(a: &Fe, b: &Fe) -> (Fe, bool) {
    let mut out = ZERO;
    let mut carry = false;
    for i in 0..4 {
        let (v, c1) = a[i].overflowing_add(b[i]);
        let (v, c2) = v.overflowing_add(u64::from(carry));
        out[i] = v;
        carry = c1 || c2;
    }
    (out, carry)
}
/// Folds a value below 2^256 into [0, P).
fn canon(mut a: Fe) -> Fe {
    for _ in 0..2 {
        if ge_p(&a) {
            a = sub_raw(&a, &P).0;
        }
    }
    a
}
fn add(a: &Fe, b: &Fe) -> Fe {
    let (mut s, carry) = add_raw(a, b);
    if carry {
        // 2^256 = 38 (mod P); the wrapped sum is small enough that this cannot carry again.
        s = add_raw(&s, &[38, 0, 0, 0]).0;
    }
    canon(s)
}
fn sub(a: &Fe, b: &Fe) -> Fe {
    let (d, borrow) = sub_raw(a, b);
    if borrow { add_raw(&d, &P).0 } else { d }
}
fn neg(a: &Fe) -> Fe {
    sub(&ZERO, a)
}
fn mul(a: &Fe, b: &Fe) -> Fe {
    let mut t = [0u64; 8];
    for i in 0..4 {
        let mut carry = 0u128;
        for j in 0..4 {
            let v = u128::from(a[i]) * u128::from(b[j]) + u128::from(t[i + j]) + carry;
            t[i + j] = v as u64;
            carry = v >> 64;
        }
        t[i + 4] = carry as u64;
    }
    let mut r = ZERO;
    let mut carry = 0u128;
    for i in 0..4 {
        let v = u128::from(t[i]) + u128::from(t[i + 4]) * 38 + carry;
        r[i] = v as u64;
        carry = v >> 64;
    }
    // carry < 39; fold it once more (2^256 = 38).
    let mut c = carry * 38;
    for limb in &mut r {
        let v = u128::from(*limb) + c;
        *limb = v as u64;
        c = v >> 64;
    }
    if c != 0 {
        r = add_raw(&r, &[38, 0, 0, 0]).0;
    }
    canon(r)
}
fn pow(base: &Fe, exponent: &Fe) -> Fe {
    let mut result = ONE;
    let mut b = *base;
    for limb in exponent {
        for bit in 0..64 {
            if (limb >> bit) & 1 == 1 {
                result = mul(&result, &b);
            }
            b = mul(&b, &b);
        }
    }
    result
}
fn small(n: u64) -> Fe {
    [n, 0, 0, 0]
}
fn is_zero(a: &Fe) -> bool {
    *a == ZERO
}
fn limbs_le(bytes: &[u8]) -> Fe {
    let mut out = ZERO;
    for (i, chunk) in bytes.chunks(8).take(4).enumerate() {
        let mut word = [0u8; 8];
        word[..chunk.len()].copy_from_slice(chunk);
        out[i] = u64::from_le_bytes(word);
    }
    out
}
fn less_than(a: &Fe, b: &Fe) -> bool {
    for i in (0..4).rev() {
        if a[i] != b[i] {
            return a[i] < b[i];
        }
    }
    false
}

struct Consts {
    d: Fe,
    sqrt_minus_one: Fe,
}
fn consts() -> &'static Consts {
    static C: OnceLock<Consts> = OnceLock::new();
    C.get_or_init(|| Consts {
        d: neg(&mul(&small(121_665), &pow(&small(121_666), &P_MINUS_2))),
        sqrt_minus_one: pow(&small(2), &P_MINUS_1_OVER_4),
    })
}

#[derive(Clone, Copy)]
struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}
const IDENTITY: Point = Point {
    x: ZERO,
    y: ONE,
    z: ONE,
    t: ZERO,
};
fn point_add(p: &Point, q: &Point) -> Point {
    let a = mul(&sub(&p.y, &p.x), &sub(&q.y, &q.x));
    let b = mul(&add(&p.y, &p.x), &add(&q.y, &q.x));
    let c = mul(&mul(&mul(&small(2), &consts().d), &p.t), &q.t);
    let d = mul(&mul(&small(2), &p.z), &q.z);
    let e = sub(&b, &a);
    let f = sub(&d, &c);
    let g = add(&d, &c);
    let h = add(&b, &a);
    Point {
        x: mul(&e, &f),
        y: mul(&g, &h),
        z: mul(&f, &g),
        t: mul(&e, &h),
    }
}
fn multiply(point: &Point, scalar: &[u64; 4]) -> Point {
    let mut result = IDENTITY;
    let mut p = *point;
    for limb in scalar {
        for bit in 0..64 {
            if (limb >> bit) & 1 == 1 {
                result = point_add(&result, &p);
            }
            p = point_add(&p, &p);
        }
    }
    result
}
fn equal(p: &Point, q: &Point) -> bool {
    is_zero(&sub(&mul(&p.x, &q.z), &mul(&q.x, &p.z)))
        && is_zero(&sub(&mul(&p.y, &q.z), &mul(&q.y, &p.z)))
}
fn decode_point(bytes: &[u8]) -> Option<Point> {
    if bytes.len() != 32 {
        return None;
    }
    let mut encoded = limbs_le(bytes);
    let sign = encoded[3] >> 63;
    encoded[3] &= 0x7fff_ffff_ffff_ffff;
    let y = encoded;
    if ge_p(&y) {
        return None;
    }
    let d = &consts().d;
    let y2 = mul(&y, &y);
    let u = sub(&y2, &ONE);
    let v = add(&mul(d, &y2), &ONE);
    let v3 = mul(&mul(&v, &v), &v);
    let v7 = mul(&mul(&v3, &v3), &v);
    let mut x = mul(&mul(&u, &v3), &pow(&mul(&u, &v7), &SQRT_EXP));
    let vxx = |x: &Fe| mul(&v, &mul(x, x));
    if vxx(&x) == neg(&u) {
        x = mul(&x, &consts().sqrt_minus_one);
    }
    if vxx(&x) != u {
        return None;
    }
    if is_zero(&x) && sign == 1 {
        return None;
    }
    if (x[0] & 1) != sign {
        x = neg(&x);
    }
    let point = Point {
        x,
        y,
        z: ONE,
        t: mul(&x, &y),
    };
    let mut eight = point;
    for _ in 0..3 {
        eight = point_add(&eight, &eight);
    }
    if equal(&eight, &IDENTITY) {
        return None;
    }
    Some(point)
}

const KEY_CACHE_LIMIT: usize = 1024;
type KeyCache = Mutex<VecDeque<([u8; 32], bool)>>;
fn key_cache() -> &'static KeyCache {
    static CACHE: OnceLock<KeyCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(VecDeque::new()))
}
fn decode_public_key_uncached(key: &[u8]) -> bool {
    decode_point(key).is_some_and(|point| equal(&multiply(&point, &L), &IDENTITY))
}
fn public_key_ok(key: &[u8]) -> bool {
    let Ok(array) = <[u8; 32]>::try_from(key) else {
        return false;
    };
    {
        let mut cache = key_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(index) = cache.iter().position(|(k, _)| *k == array)
            && let Some(entry) = cache.remove(index)
        {
            let verdict = entry.1;
            cache.push_back(entry);
            return verdict;
        }
    }
    let verdict = decode_public_key_uncached(&array);
    let mut cache = key_cache().lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() >= KEY_CACHE_LIMIT {
        cache.pop_front();
    }
    cache.push_back((array, verdict));
    verdict
}

/// Registration and verification share canonical, nonidentity prime-order key checks.
pub fn strict_ed25519_key(key: &[u8]) -> bool {
    public_key_ok(key)
}

/// Not a signature verdict: follow with the native verifier on the exact same bytes.
pub fn strict_ed25519_precheck(key: &[u8], sig: &[u8]) -> Result<(), StrictEd25519Refusal> {
    if key.len() != 32 || sig.len() != 64 {
        return Err(StrictEd25519Refusal::Malformed);
    }
    if !public_key_ok(key)
        || decode_point(&sig[..32]).is_none()
        || !less_than(&limbs_le(&sig[32..]), &L)
    {
        return Err(StrictEd25519Refusal::Malformed);
    }
    Ok(())
}

pub fn strict_ed25519_verify(key: &[u8], sig: &[u8], msg: &[u8]) -> StrictEd25519Verdict {
    strict_ed25519_precheck(key, sig)?;
    UnparsedPublicKey::new(&ED25519, key)
        .verify(msg, sig)
        .map_err(|_| StrictEd25519Refusal::Signature)
}

/// Strict registration policy, not RFC 7748's permissive masking/reduction of received u.
pub fn strict_x25519_key(key: &[u8]) -> bool {
    if key.len() != 32 || key[31] & 0x80 != 0 {
        return false;
    }
    let u = limbs_le(key);
    if ge_p(&u) {
        return false;
    }
    let p_minus_1 = [P[0] - 1, P[1], P[2], P[3]];
    !(is_zero(&u) || u == ONE || u == p_minus_1 || X25519_ORDER8.contains(&u))
}

/// Call after native X25519 derivation and before KDF; never use a refused secret.
pub fn check_shared_secret(secret: &[u8]) -> bool {
    secret.len() == 32 && secret.iter().any(|b| *b != 0)
}
