//! The keys of the line-layout cache.
//!
//! The reference keeps these in `gpui_engine_default`, together with the cache that
//! uses them. That crate does not exist here yet, and the cache itself cannot leave
//! `gpui` while `LineLayout` is still pinned there, so `gpui_engine` holds the keys
//! on their behalf.

use crate::FontRun;
use gpui_shared_string::SharedString;
use gpui_types::Pixels;
use smallvec::SmallVec;
use std::{
    borrow::Borrow,
    hash::{Hash, Hasher},
    sync::Arc,
};

/// A borrowed view of a shaped-line cache key, so the cache can be probed without
/// allocating an owned key.
pub trait AsCacheKeyRef {
    /// The borrowed key.
    fn as_cache_key_ref(&self) -> CacheKeyRef<'_>;
}

/// A cache key for a shaped line, keyed on the text and the layout parameters.
#[derive(Clone, Debug, Eq)]
pub struct CacheKey {
    /// The text that was shaped.
    pub text: SharedString,
    /// The font size the line was shaped at.
    pub font_size: Pixels,
    /// The font runs the text was shaped with.
    pub runs: SmallVec<[FontRun; 1]>,
    /// The width the line was wrapped to, if any.
    pub wrap_width: Option<Pixels>,
    /// The force width applied to the line, if any.
    pub force_width: Option<Pixels>,
}

/// A borrowed [`CacheKey`].
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct CacheKeyRef<'a> {
    /// The text that was shaped.
    pub text: &'a str,
    /// The font size the line was shaped at.
    pub font_size: Pixels,
    /// The font runs the text was shaped with.
    pub runs: &'a [FontRun],
    /// The width the line was wrapped to, if any.
    pub wrap_width: Option<Pixels>,
    /// The force width applied to the line, if any.
    pub force_width: Option<Pixels>,
}

/// A content-addressable shaped-line cache key, keyed on a caller-provided text hash
/// instead of the text itself.
#[derive(Clone, Debug)]
pub struct HashedCacheKey {
    /// The caller-provided hash of the text.
    pub text_hash: u64,
    /// The UTF-8 byte length of the text.
    pub text_len: usize,
    /// The font size the line was shaped at.
    pub font_size: Pixels,
    /// The font runs the text was shaped with.
    pub runs: SmallVec<[FontRun; 1]>,
    /// The width the line was wrapped to, if any.
    pub wrap_width: Option<Pixels>,
    /// The force width applied to the line, if any.
    pub force_width: Option<Pixels>,
}

/// A borrowed [`HashedCacheKey`].
#[derive(Copy, Clone)]
pub struct HashedCacheKeyRef<'a> {
    /// The caller-provided hash of the text.
    pub text_hash: u64,
    /// The UTF-8 byte length of the text.
    pub text_len: usize,
    /// The font size the line was shaped at.
    pub font_size: Pixels,
    /// The font runs the text was shaped with.
    pub runs: &'a [FontRun],
    /// The width the line was wrapped to, if any.
    pub wrap_width: Option<Pixels>,
    /// The force width applied to the line, if any.
    pub force_width: Option<Pixels>,
}

impl PartialEq for dyn AsCacheKeyRef + '_ {
    fn eq(&self, other: &dyn AsCacheKeyRef) -> bool {
        self.as_cache_key_ref() == other.as_cache_key_ref()
    }
}

impl PartialEq for HashedCacheKey {
    fn eq(&self, other: &Self) -> bool {
        self.text_hash == other.text_hash
            && self.text_len == other.text_len
            && self.font_size == other.font_size
            && self.runs.as_slice() == other.runs.as_slice()
            && self.wrap_width == other.wrap_width
            && self.force_width == other.force_width
    }
}

impl Eq for HashedCacheKey {}

impl Hash for HashedCacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.text_hash.hash(state);
        self.text_len.hash(state);
        self.font_size.hash(state);
        self.runs.as_slice().hash(state);
        self.wrap_width.hash(state);
        self.force_width.hash(state);
    }
}

impl PartialEq for HashedCacheKeyRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.text_hash == other.text_hash
            && self.text_len == other.text_len
            && self.font_size == other.font_size
            && self.runs == other.runs
            && self.wrap_width == other.wrap_width
            && self.force_width == other.force_width
    }
}

impl Eq for HashedCacheKeyRef<'_> {}

impl Hash for HashedCacheKeyRef<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.text_hash.hash(state);
        self.text_len.hash(state);
        self.font_size.hash(state);
        self.runs.hash(state);
        self.wrap_width.hash(state);
        self.force_width.hash(state);
    }
}

impl Eq for dyn AsCacheKeyRef + '_ {}

impl Hash for dyn AsCacheKeyRef + '_ {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_cache_key_ref().hash(state)
    }
}

impl AsCacheKeyRef for CacheKey {
    fn as_cache_key_ref(&self) -> CacheKeyRef<'_> {
        CacheKeyRef {
            text: &self.text,
            font_size: self.font_size,
            runs: self.runs.as_slice(),
            wrap_width: self.wrap_width,
            force_width: self.force_width,
        }
    }
}

impl PartialEq for CacheKey {
    fn eq(&self, other: &Self) -> bool {
        self.as_cache_key_ref().eq(&other.as_cache_key_ref())
    }
}

impl Hash for CacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_cache_key_ref().hash(state);
    }
}

impl<'a> Borrow<dyn AsCacheKeyRef + 'a> for Arc<CacheKey> {
    fn borrow(&self) -> &(dyn AsCacheKeyRef + 'a) {
        self.as_ref() as &dyn AsCacheKeyRef
    }
}

impl AsCacheKeyRef for CacheKeyRef<'_> {
    fn as_cache_key_ref(&self) -> CacheKeyRef<'_> {
        *self
    }
}
