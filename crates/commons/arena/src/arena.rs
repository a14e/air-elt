//! Generic arena with u16 indexing for compact, cache-local program layout.

use std::hash::{Hash, Hasher};
use std::marker::PhantomData;

/// Index of a single slot in an [`Arena<T>`]. Tagged with the element type
/// `T` so a reference into one arena cannot be used to index another of a
/// different type — a class of bugs caught at compile time.
///
/// The marker is `PhantomData<fn() -> T>` (not `PhantomData<T>`): the handle
/// neither owns nor borrows a `T`, stays `Copy`/`Send`/`Sync` regardless of
/// `T`, and the trait impls below are hand-written so they impose no bounds
/// on `T` (a `#[derive]` would spuriously require `T: Copy`, `T: Eq`, …).
pub struct ArenaRef<T> {
    index: u16,
    _marker: PhantomData<fn() -> T>,
}

impl<T> ArenaRef<T> {
    fn new(index: u16) -> Self {
        Self {
            index,
            _marker: PhantomData,
        }
    }

    /// The raw slot index, exposed so downstream crates can serialize and
    /// inspect references.
    pub fn index(&self) -> u16 {
        self.index
    }
}

impl<T> Clone for ArenaRef<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for ArenaRef<T> {}
impl<T> PartialEq for ArenaRef<T> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index
    }
}
impl<T> Eq for ArenaRef<T> {}
impl<T> Hash for ArenaRef<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.index.hash(state);
    }
}
impl<T> std::fmt::Debug for ArenaRef<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ArenaRef").field(&self.index).finish()
    }
}

/// A contiguous run of slots in an [`Arena<T>`] (e.g. a function's argument
/// list). Type-tagged with `T` for the same reason as [`ArenaRef`].
pub struct ArenaSlice<T> {
    start: u16,
    len: u16,
    _marker: PhantomData<fn() -> T>,
}

impl<T> ArenaSlice<T> {
    fn new(start: u16, len: u16) -> Self {
        Self {
            start,
            len,
            _marker: PhantomData,
        }
    }

    /// Index of the first slot in the run.
    pub fn start(&self) -> u16 {
        self.start
    }

    /// Number of slots in the run.
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether the run is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<T> Clone for ArenaSlice<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for ArenaSlice<T> {}
impl<T> PartialEq for ArenaSlice<T> {
    fn eq(&self, other: &Self) -> bool {
        self.start == other.start && self.len == other.len
    }
}
impl<T> Eq for ArenaSlice<T> {}
impl<T> std::fmt::Debug for ArenaSlice<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArenaSlice")
            .field("start", &self.start)
            .field("len", &self.len)
            .finish()
    }
}

impl<T> ArenaSlice<T> {
    /// Appends `value` to this run, growing it in place — Go-`append`-style.
    ///
    /// Succeeds only while the run is still at `arena`'s tail (its end equals
    /// `arena.len()`). If another allocation has pushed past it, growing would
    /// require relocation, which is refused with [`SlicePushError::NotAtTail`]
    /// rather than silently moving the run. The arena must be the one this
    /// slice was opened from.
    pub fn push(&mut self, arena: &mut Arena<T>, value: T) -> Result<(), SlicePushError> {
        let end = self.start as usize + self.len as usize;
        if end != arena.len() {
            return Err(SlicePushError::NotAtTail);
        }
        // Delegate the append (and its u16 overflow check) to `alloc` rather
        // than touching the arena's internals or duplicating the bound.
        arena.alloc(value).map_err(|_| SlicePushError::Overflow)?;
        self.len += 1;
        Ok(())
    }
}

/// Raised when an allocation would push the arena past `u16::MAX` slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArenaOverflow;

impl std::fmt::Display for ArenaOverflow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "arena capacity exceeded: more than {} slots", u16::MAX)
    }
}

impl std::error::Error for ArenaOverflow {}

/// Why an in-place [`ArenaSlice::push`] was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlicePushError {
    /// The run is no longer at the arena's tail (something was appended
    /// after it), so growing it in place would require relocating it.
    /// Relocation is deliberately unsupported — it would leave dead slots
    /// and break the execution-order layout the arena exists to preserve.
    NotAtTail,
    /// The arena is full (`u16::MAX` slots).
    Overflow,
}

impl std::fmt::Display for SlicePushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SlicePushError::NotAtTail => write!(
                f,
                "cannot grow slice in place: it is no longer at the arena tail \
                 (would require relocation)"
            ),
            SlicePushError::Overflow => {
                write!(f, "arena capacity exceeded: more than {} slots", u16::MAX)
            }
        }
    }
}

impl std::error::Error for SlicePushError {}

/// A generic, dependency-free arena addressed by `u16` indices. Items are
/// stored contiguously so a lowered program lays out compactly and stays
/// cache-local. The arena holds at most `u16::MAX` slots.
#[derive(Debug)]
pub struct Arena<T> {
    items: Vec<T>,
}

impl<T> Arena<T> {
    /// Builds an empty arena.
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    /// Builds an empty arena with room for `cap` items reserved up front.
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            items: Vec::with_capacity(cap),
        }
    }

    /// Appends a single item and returns its reference. Errors when the new
    /// length would exceed `u16::MAX`.
    pub fn alloc(&mut self, value: T) -> Result<ArenaRef<T>, ArenaOverflow> {
        let index = self.items.len();
        if index >= u16::MAX as usize {
            return Err(ArenaOverflow);
        }
        self.items.push(value);
        Ok(ArenaRef::new(index as u16))
    }

    /// Opens an empty run at the current tail. Grow it in place with
    /// [`ArenaSlice::push`] for as long as it stays at the tail — a
    /// Go-`append`-style builder that never relocates.
    pub fn open_slice(&self) -> ArenaSlice<T> {
        ArenaSlice::new(self.items.len() as u16, 0)
    }

    /// Borrows the item at `r`.
    pub fn get(&self, r: ArenaRef<T>) -> &T {
        &self.items[r.index as usize]
    }

    /// Mutably borrows the item at `r`.
    pub fn get_mut(&mut self, r: ArenaRef<T>) -> &mut T {
        &mut self.items[r.index as usize]
    }

    /// Borrows the contiguous run described by `s`.
    pub fn slice(&self, s: ArenaSlice<T>) -> &[T] {
        let start = s.start as usize;
        let end = start + s.len as usize;
        &self.items[start..end]
    }

    /// Iterate every stored item in allocation order.
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.items.iter()
    }

    /// Number of items currently stored.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the arena holds no items.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

impl<T> Default for Arena<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn alloc_returns_increasing_indices() {
        let mut arena: Arena<&str> = Arena::new();

        let first = arena.alloc("a").unwrap();
        let second = arena.alloc("b").unwrap();
        let third = arena.alloc("c").unwrap();

        assert_eq!(first.index(), 0);
        assert_eq!(second.index(), 1);
        assert_eq!(third.index(), 2);
        assert_eq!(arena.len(), 3);
        assert!(!arena.is_empty());
    }

    #[test]
    fn get_round_trips_values() {
        let mut arena: Arena<i32> = Arena::with_capacity(2);

        let a = arena.alloc(10).unwrap();
        let b = arena.alloc(20).unwrap();

        assert_eq!(*arena.get(a), 10);
        assert_eq!(*arena.get(b), 20);
    }

    #[test]
    fn open_slice_and_push_build_run_in_order() {
        let mut arena: Arena<i32> = Arena::new();

        // A leading single alloc so the run does not start at 0.
        arena.alloc(99).unwrap();
        let mut run = arena.open_slice();
        for v in [1, 2, 3, 4] {
            run.push(&mut arena, v).unwrap();
        }

        assert_eq!(run.start(), 1);
        assert_eq!(run.len(), 4);
        assert!(!run.is_empty());
        assert_eq!(arena.slice(run), &[1, 2, 3, 4]);
        assert_eq!(arena.len(), 5);
    }

    #[test]
    fn open_slice_with_no_push_is_empty() {
        let mut arena: Arena<i32> = Arena::new();
        arena.alloc(7).unwrap();

        let run = arena.open_slice();

        assert_eq!(run.start(), 1);
        assert_eq!(run.len(), 0);
        assert!(run.is_empty());
        assert_eq!(arena.slice(run), &[] as &[i32]);
        assert_eq!(arena.len(), 1);
    }

    #[test]
    fn push_refused_when_run_left_the_tail() {
        let mut arena: Arena<i32> = Arena::new();
        let mut run = arena.open_slice();
        run.push(&mut arena, 1).unwrap();

        // Another allocation moves the tail past the run.
        arena.alloc(99).unwrap();

        // Growing in place would require relocation — refused.
        let err = run.push(&mut arena, 2).unwrap_err();
        assert_eq!(err, SlicePushError::NotAtTail);
        // The run is unchanged and the stray alloc is intact.
        assert_eq!(run.len(), 1);
        assert_eq!(arena.slice(run), &[1]);
        assert_eq!(arena.len(), 2);
    }

    #[test]
    fn push_overflow_at_capacity() {
        let mut arena: Arena<u8> = Arena::with_capacity(u16::MAX as usize);
        for _ in 0..(u16::MAX - 1) {
            arena.alloc(0).unwrap();
        }
        // Open a run at the tail and fill the single remaining slot.
        let mut run = arena.open_slice();
        run.push(&mut arena, 1).unwrap();
        assert_eq!(arena.len(), u16::MAX as usize);
        // The next push overflows the u16 budget.
        assert_eq!(run.push(&mut arena, 2), Err(SlicePushError::Overflow));
    }

    #[test]
    fn get_mut_mutates() {
        let mut arena: Arena<i32> = Arena::new();
        let r = arena.alloc(5).unwrap();

        *arena.get_mut(r) = 42;

        assert_eq!(*arena.get(r), 42);
    }

    #[test]
    fn debug_renders_items() {
        let mut arena: Arena<i32> = Arena::new();
        arena.alloc(7).unwrap();
        arena.alloc(8).unwrap();
        let rendered = format!("{arena:?}");
        assert!(rendered.contains('7') && rendered.contains('8'));
    }

    #[test]
    fn new_arena_is_empty() {
        let arena: Arena<i32> = Arena::default();
        assert_eq!(arena.len(), 0);
        assert!(arena.is_empty());
    }

    #[test]
    fn alloc_errors_when_exceeding_u16_max() {
        // Fill exactly to u16::MAX with a tight u8 loop, then assert the
        // next alloc overflows.
        let mut arena: Arena<u8> = Arena::with_capacity(u16::MAX as usize);
        for _ in 0..u16::MAX {
            arena.alloc(0).unwrap();
        }
        assert_eq!(arena.len(), u16::MAX as usize);

        let overflow = arena.alloc(0);
        assert_eq!(overflow, Err(ArenaOverflow));
        assert_eq!(arena.len(), u16::MAX as usize, "failed alloc must not grow");
    }

    #[test]
    fn overflow_error_displays_and_is_std_error() {
        let err = ArenaOverflow;
        let text = err.to_string();
        assert!(text.contains(&u16::MAX.to_string()));
        // Exercise the std::error::Error impl.
        let _as_error: &dyn std::error::Error = &err;
    }
}
