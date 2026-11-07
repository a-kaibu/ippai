//! The kernel's state as bytes, for the processes that share it (kern::with through the host's
//! shared buffer): little-endian numbers, lengths before sequences. Only what this build wrote
//! is read back (every process of a host runs the same sake), so there is no versioning.

use std::collections::{BTreeMap, VecDeque};

pub trait Enc: Sized {
    fn put(&self, w: &mut Vec<u8>);
    fn get(r: &mut &[u8]) -> Option<Self>;
}

fn take<'a>(r: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    if r.len() < n {
        return None;
    }
    let (a, b) = r.split_at(n);
    *r = b;
    Some(a)
}

macro_rules! num {
    ($($t:ty),*) => {$(
        impl Enc for $t {
            fn put(&self, w: &mut Vec<u8>) {
                w.extend_from_slice(&self.to_le_bytes());
            }
            fn get(r: &mut &[u8]) -> Option<Self> {
                Some(<$t>::from_le_bytes(take(r, std::mem::size_of::<$t>())?.try_into().ok()?))
            }
        }
    )*};
}
num!(u8, u32, u64, i32);

impl Enc for bool {
    fn put(&self, w: &mut Vec<u8>) {
        w.push(*self as u8);
    }
    fn get(r: &mut &[u8]) -> Option<Self> {
        Some(u8::get(r)? != 0)
    }
}
impl Enc for usize {
    fn put(&self, w: &mut Vec<u8>) {
        (*self as u64).put(w)
    }
    fn get(r: &mut &[u8]) -> Option<Self> {
        Some(u64::get(r)? as usize)
    }
}
impl Enc for String {
    fn put(&self, w: &mut Vec<u8>) {
        self.len().put(w);
        w.extend_from_slice(self.as_bytes());
    }
    fn get(r: &mut &[u8]) -> Option<Self> {
        let n = usize::get(r)?;
        String::from_utf8(take(r, n)?.to_vec()).ok()
    }
}
impl<T: Enc> Enc for Vec<T> {
    fn put(&self, w: &mut Vec<u8>) {
        self.len().put(w);
        self.iter().for_each(|x| x.put(w));
    }
    fn get(r: &mut &[u8]) -> Option<Self> {
        let n = usize::get(r)?;
        if n > r.len() {
            return None;
        }
        (0..n).map(|_| T::get(r)).collect()
    }
}
impl<T: Enc> Enc for VecDeque<T> {
    fn put(&self, w: &mut Vec<u8>) {
        self.len().put(w);
        self.iter().for_each(|x| x.put(w));
    }
    fn get(r: &mut &[u8]) -> Option<Self> {
        Some(Vec::<T>::get(r)?.into())
    }
}
impl<T: Enc> Enc for Option<T> {
    fn put(&self, w: &mut Vec<u8>) {
        match self {
            None => w.push(0),
            Some(x) => {
                w.push(1);
                x.put(w)
            }
        }
    }
    fn get(r: &mut &[u8]) -> Option<Self> {
        Some(if bool::get(r)? {
            Some(T::get(r)?)
        } else {
            None
        })
    }
}
impl<A: Enc, B: Enc> Enc for (A, B) {
    fn put(&self, w: &mut Vec<u8>) {
        self.0.put(w);
        self.1.put(w);
    }
    fn get(r: &mut &[u8]) -> Option<Self> {
        Some((A::get(r)?, B::get(r)?))
    }
}
impl<K: Enc + Ord, V: Enc> Enc for BTreeMap<K, V> {
    fn put(&self, w: &mut Vec<u8>) {
        self.len().put(w);
        for (k, v) in self {
            k.put(w);
            v.put(w);
        }
    }
    fn get(r: &mut &[u8]) -> Option<Self> {
        let n = usize::get(r)?;
        if n > r.len() {
            return None;
        }
        (0..n).map(|_| Some((K::get(r)?, V::get(r)?))).collect()
    }
}

/// `Enc` for a struct, field by field.
#[macro_export]
macro_rules! enc_struct {
    ($t:ident { $($f:ident),* $(,)? }) => {
        impl $crate::kern::enc::Enc for $t {
            fn put(&self, w: &mut Vec<u8>) {
                $( self.$f.put(w); )*
            }
            fn get(r: &mut &[u8]) -> Option<Self> {
                Some($t { $( $f: $crate::kern::enc::Enc::get(r)?, )* })
            }
        }
    };
}
