//! Generic arena with u16 indexing for compact, cache-local program layout.

/// Index of a single slot in an [`Arena`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArenaRef(u16);

impl ArenaRef {
    /// The raw slot index, exposed so downstream crates can serialize and
    /// inspect references.
    pub fn index(&self) -> u16 {
        self.0
    }
}

/// A contiguous run of slots in an [`Arena`] (e.g. a function's argument list).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArenaSlice {
    start: u16,
    len: u16,
}

impl ArenaSlice {
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

/// Raised when an allocation would push the arena past `u16::MAX` slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArenaOverflow;

impl std::fmt::Display for ArenaOverflow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "arena capacity exceeded: more than {} slots", u16::MAX)
    }
}

impl std::error::Error for ArenaOverflow {}

/// A generic, dependency-free arena addressed by `u16` indices. Items are
/// stored contiguously so a lowered program lays out compactly and stays
/// cache-local. The arena holds at most `u16::MAX` slots.
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
    pub fn alloc(&mut self, value: T) -> Result<ArenaRef, ArenaOverflow> {
        let index = self.items.len();
        if index >= u16::MAX as usize {
            return Err(ArenaOverflow);
        }
        self.items.push(value);
        Ok(ArenaRef(index as u16))
    }

    /// Appends a contiguous run of items and returns the slice spanning them.
    /// An empty input yields a zero-length slice at the current length.
    /// Errors when the run would push the length past `u16::MAX`.
    pub fn alloc_slice(
        &mut self,
        values: impl IntoIterator<Item = T>,
    ) -> Result<ArenaSlice, ArenaOverflow> {
        let start = self.items.len();
        if start > u16::MAX as usize {
            return Err(ArenaOverflow);
        }

        let mut count: usize = 0;
        for value in values {
            let next_len = start + count + 1;
            if next_len > u16::MAX as usize {
                // Roll back the partial run so the arena stays consistent.
                self.items.truncate(start);
                return Err(ArenaOverflow);
            }
            self.items.push(value);
            count += 1;
        }

        Ok(ArenaSlice {
            start: start as u16,
            len: count as u16,
        })
    }

    /// Borrows the item at `r`.
    pub fn get(&self, r: ArenaRef) -> &T {
        &self.items[r.0 as usize]
    }

    /// Mutably borrows the item at `r`.
    pub fn get_mut(&mut self, r: ArenaRef) -> &mut T {
        &mut self.items[r.0 as usize]
    }

    /// Borrows the contiguous run described by `s`.
    pub fn slice(&self, s: ArenaSlice) -> &[T] {
        let start = s.start as usize;
        let end = start + s.len as usize;
        &self.items[start..end]
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
    fn alloc_slice_stores_run_in_order() {
        let mut arena: Arena<i32> = Arena::new();

        // A leading single alloc so the slice does not start at 0.
        arena.alloc(99).unwrap();
        let run = arena.alloc_slice([1, 2, 3, 4]).unwrap();

        assert_eq!(run.start(), 1);
        assert_eq!(run.len(), 4);
        assert!(!run.is_empty());
        assert_eq!(arena.slice(run), &[1, 2, 3, 4]);
        assert_eq!(arena.len(), 5);
    }

    #[test]
    fn alloc_slice_empty_input_yields_zero_length_slice() {
        let mut arena: Arena<i32> = Arena::new();
        arena.alloc(7).unwrap();

        let run = arena.alloc_slice(std::iter::empty::<i32>()).unwrap();

        assert_eq!(run.start(), 1);
        assert_eq!(run.len(), 0);
        assert!(run.is_empty());
        assert_eq!(arena.slice(run), &[] as &[i32]);
        assert_eq!(arena.len(), 1);
    }

    #[test]
    fn get_mut_mutates() {
        let mut arena: Arena<i32> = Arena::new();
        let r = arena.alloc(5).unwrap();

        *arena.get_mut(r) = 42;

        assert_eq!(*arena.get(r), 42);
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
    fn alloc_slice_errors_and_rolls_back_on_overflow() {
        // Leave room for only one more slot, then push a two-item run.
        let mut arena: Arena<u8> = Arena::with_capacity(u16::MAX as usize);
        for _ in 0..(u16::MAX - 1) {
            arena.alloc(0).unwrap();
        }
        let before = arena.len();

        let result = arena.alloc_slice([1, 2]);

        assert_eq!(result, Err(ArenaOverflow));
        assert_eq!(before, arena.len(), "overflowing run must be rolled back");

        // The single remaining slot can still be filled by a one-item run.
        let run = arena.alloc_slice([7]).unwrap();
        assert_eq!(run.len(), 1);
        assert_eq!(arena.len(), u16::MAX as usize);
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
