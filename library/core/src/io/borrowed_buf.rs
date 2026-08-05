#![unstable(feature = "core_io_borrowed_buf", issue = "117693")]

use crate::fmt::{self, Debug, Formatter};
use crate::marker::PhantomData;
use crate::mem::{ManuallyDrop, MaybeUninit};
use crate::ptr::{self, NonNull};
use crate::slice;

/// A borrowed buffer of initially uninitialized elements, which is incrementally filled.
///
/// This type makes it safer to work with `MaybeUninit` buffers, such as to read into a buffer
/// without having to initialize it first. It tracks the region of elements that have been filled
/// and whether the unfilled region was initialized.
///
/// In summary, the contents of the buffer can be visualized as:
/// ```not_rust
/// [                capacity                ]
/// [ filled | unfilled (may be initialized) ]
/// ```
///
/// A `BorrowedBuf` is created around some existing elements (or capacity for elements) via a unique
/// reference (`&mut`). The `BorrowedBuf` can be configured (e.g., using `clear` or `set_init`), but
/// cannot be directly written. To write into the buffer, use `unfilled` to create a
/// `BorrowedCursor`. The cursor has write-only access to the unfilled portion of the buffer (you
/// can think of it as a write-only iterator).
///
/// The lifetime `'data` is a bound on the lifetime of the underlying elements.
///
/// The type is most commonly used to manage bytes, but can manage any type of elements.
pub struct BorrowedBuf<'data, T> {
    /// The buffer's underlying elements.
    buf_ptr: NonNull<MaybeUninit<T>>,
    /// The capacity of the buffer's underlying elements.
    capacity: usize,
    /// The number of elements of `self.buf` that are known to be filled.
    filled: usize,
    /// Whether the entire unfilled part of `self.buf` has explicitly been initialized.
    init: bool,
    /// Use `'data` and mark `T` as invariant.
    _marker: PhantomData<&'data mut [MaybeUninit<T>]>,
}

impl<T> Debug for BorrowedBuf<'_, T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("BorrowedBuf")
            .field("init", &self.init)
            .field("filled", &self.filled)
            .field("capacity", &self.capacity)
            .finish()
    }
}

/// Creates a new `BorrowedBuf` from a fully initialized slice.
impl<'data, T: Copy> From<&'data mut [T]> for BorrowedBuf<'data, T> {
    #[inline]
    fn from(slice: &'data mut [T]) -> BorrowedBuf<'data, T> {
        // SAFETY: no initialized element is ever uninitialized as per `BorrowedBuf`'s invariant
        let uninit_slice = unsafe { &mut *(slice as *mut [T] as *mut [MaybeUninit<T>]) };
        Self::from(uninit_slice)
    }
}

/// Creates a new `BorrowedBuf` from an uninitialized buffer.
impl<'data, T: Copy> From<&'data mut [MaybeUninit<T>]> for BorrowedBuf<'data, T> {
    #[inline]
    fn from(buf: &'data mut [MaybeUninit<T>]) -> BorrowedBuf<'data, T> {
        let capacity = buf.len();
        BorrowedBuf {
            buf_ptr: NonNull::from(buf).cast(),
            capacity,
            filled: 0,
            init: false,
            _marker: PhantomData,
        }
    }
}

/// Creates a new `BorrowedBuf` from a cursor.
///
/// Use `BorrowedCursor::with_unfilled_buf` instead for a safer alternative.
impl<'data, T: Copy> From<BorrowedCursor<'data, T>> for BorrowedBuf<'data, T> {
    #[inline]
    fn from(buf: BorrowedCursor<'data, T>) -> BorrowedBuf<'data, T> {
        let filled = buf.borrowed_buf().filled;
        let init = buf.borrowed_buf().init;
        let capacity = buf.borrowed_buf().capacity - filled;
        BorrowedBuf {
            // SAFETY: `filled` is always within the buffer's bounds.
            buf_ptr: unsafe { buf.buf.add(filled) },
            capacity,
            filled: 0,
            init,
            _marker: PhantomData,
        }
    }
}

impl<'data, T> BorrowedBuf<'data, T> {
    fn buf(&self) -> &[MaybeUninit<T>] {
        // SAFETY: `buf_ptr` points to `capacity` elements that this buffer borrows exclusively.
        unsafe { slice::from_raw_parts(self.buf_ptr.as_ptr(), self.capacity) }
    }

    fn buf_mut(&mut self) -> &mut [MaybeUninit<T>] {
        // SAFETY: `buf_ptr` points to `capacity` elements that this buffer borrows exclusively.
        unsafe { slice::from_raw_parts_mut(self.buf_ptr.as_ptr(), self.capacity) }
    }

    /// Returns the total capacity of the buffer.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns the length of the filled part of the buffer.
    #[inline]
    pub fn len(&self) -> usize {
        self.filled
    }

    /// Returns `true` if the buffer is initialized.
    #[unstable(feature = "borrowed_buf_init", issue = "160476")]
    #[inline]
    pub fn is_init(&self) -> bool {
        self.init
    }
}

impl<'data, T: Copy> BorrowedBuf<'data, T> {
    /// Returns a shared reference to the filled portion of the buffer.
    #[inline]
    pub fn filled(&self) -> &[T] {
        // SAFETY: We only slice the filled part of the buffer, which is always valid
        unsafe {
            let buf = self.buf().get_unchecked(..self.filled);
            buf.assume_init_ref()
        }
    }

    /// Returns a mutable reference to the filled portion of the buffer.
    #[inline]
    pub fn filled_mut(&mut self) -> &mut [T] {
        let filled = self.filled;
        // SAFETY: We only slice the filled part of the buffer, which is always valid
        unsafe {
            let buf = self.buf_mut().get_unchecked_mut(..filled);
            buf.assume_init_mut()
        }
    }

    /// Returns a shared reference to the filled portion of the buffer with its original lifetime.
    #[inline]
    pub fn into_filled(self) -> &'data [T] {
        let this = ManuallyDrop::new(self);
        // SAFETY: we are consuming `self`, thus we can extend the lifetime back to `'data`.
        unsafe { &*ptr::from_ref(this.filled()) }
    }

    /// Returns a mutable reference to the filled portion of the buffer with its original lifetime.
    #[inline]
    pub fn into_filled_mut(self) -> &'data mut [T] {
        let mut this = ManuallyDrop::new(self);
        // SAFETY: we are consuming `self`, thus we can extend the lifetime back to `'data`.
        unsafe { &mut *ptr::from_mut(this.filled_mut()) }
    }

    /// Returns a cursor over the unfilled part of the buffer.
    #[inline]
    pub fn unfilled<'this>(&'this mut self) -> BorrowedCursor<'this, T> {
        let borrowed_buf = NonNull::from_mut(self);
        // SAFETY: `borrowed_buf` was created from `&mut self` right now.
        let buf = unsafe { (*borrowed_buf.as_ptr()).buf_ptr };
        BorrowedCursor { buf, borrowed_buf }
    }

    /// Clears the buffer, resetting the filled region to empty.
    ///
    /// The contents of the buffer are not modified.
    #[inline]
    pub fn clear(&mut self) -> &mut Self {
        self.filled = 0;
        self
    }

    /// Asserts that the unfilled part of the buffer is initialized.
    ///
    /// # Safety
    ///
    /// All the elements of the buffer must be initialized.
    #[unstable(feature = "borrowed_buf_init", issue = "160476")]
    #[inline]
    pub unsafe fn set_init(&mut self) -> &mut Self {
        self.init = true;
        self
    }
}

/// A writeable view of the unfilled portion of a [`BorrowedBuf`].
///
/// The unfilled portion may be uninitialized; see [`BorrowedBuf`] for details.
///
/// Data can be written directly to the cursor by using [`append`](BorrowedCursor::append) or
/// indirectly by getting a slice of part or all of the cursor and writing into the slice. In the
/// indirect case, the caller must call [`advance`](BorrowedCursor::advance) after writing to inform
/// the cursor how many elements have been written.
///
/// Once elements are written to the cursor, they become part of the filled portion of the
/// underlying `BorrowedBuf` and can no longer be accessed or re-written by the cursor. In other
/// words, the cursor tracks the unfilled part of the underlying `BorrowedBuf`.
///
/// The lifetime `'a` is a bound on the lifetime of the underlying buffer (which means it is a bound
/// on the elements in that buffer by transitivity).
pub struct BorrowedCursor<'a, T> {
    /// The start of the elements of the buffer this cursor was created from.
    buf: NonNull<MaybeUninit<T>>,
    /// The `BorrowedBuf` this cursor was created from.
    // Safety invariants: we don't access `buf`'s pointee through this pointer.
    borrowed_buf: NonNull<BorrowedBuf<'a, T>>,
}

impl<T> Debug for BorrowedCursor<'_, T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("BorrowedCursor").field("buf", &self.borrowed_buf()).finish()
    }
}

// Helpers to access underlying buffer state.
impl<'a, T> BorrowedCursor<'a, T> {
    #[inline]
    fn borrowed_buf(&self) -> &BorrowedBuf<'a, T> {
        // SAFETY: This pointer is valid for reads and writes.
        unsafe { self.borrowed_buf.as_ref() }
    }

    #[inline]
    fn borrowed_buf_mut(&mut self) -> &mut BorrowedBuf<'a, T> {
        // SAFETY: This pointer is valid for reads and writes.
        unsafe { self.borrowed_buf.as_mut() }
    }

    #[inline]
    fn buf_mut(&mut self) -> &mut [MaybeUninit<T>] {
        let len = self.borrowed_buf().capacity;
        // SAFETY: `buf` points to `len` elements that this cursor borrows exclusively.
        unsafe { slice::from_raw_parts_mut(self.buf.as_ptr(), len) }
    }

    /// # Safety
    ///
    /// In case of `true` all the elements of the cursor must be initialized.
    #[inline]
    unsafe fn set_buf_init(&mut self, init: bool) {
        self.borrowed_buf_mut().init = init;
    }

    /// # Safety
    ///
    /// The next `n` elements of the cursor must be initialized.
    #[inline]
    unsafe fn add_filled(&mut self, n: usize) {
        self.borrowed_buf_mut().filled += n;
    }
}

impl<'a, T: Copy> BorrowedCursor<'a, T> {
    /// Reborrows this cursor by cloning it with a smaller lifetime.
    ///
    /// Since a cursor maintains unique access to its underlying buffer, the borrowed cursor is
    /// not accessible while the new cursor exists.
    #[inline]
    pub fn reborrow<'this>(&'this mut self) -> BorrowedCursor<'this, T> {
        BorrowedCursor { buf: self.buf, borrowed_buf: self.borrowed_buf }
    }

    /// Returns the available space in the cursor.
    #[inline]
    pub fn capacity(&self) -> usize {
        // The subtraction cannot underflow by invariant of this type.
        self.borrowed_buf().capacity() - self.borrowed_buf().filled
    }

    /// Returns the number of elements written to the `BorrowedBuf` this cursor was created from.
    ///
    /// In particular, the count returned is shared by all reborrows of the cursor.
    #[inline]
    pub fn written(&self) -> usize {
        self.borrowed_buf().filled
    }

    /// Returns `true` if the buffer is initialized.
    #[unstable(feature = "borrowed_buf_init", issue = "160476")]
    #[inline]
    pub fn is_init(&self) -> bool {
        self.borrowed_buf().init
    }

    /// Set the buffer as fully initialized.
    ///
    /// # Safety
    ///
    /// All the elements of the cursor must be initialized.
    #[unstable(feature = "borrowed_buf_init", issue = "160476")]
    #[inline]
    pub unsafe fn set_init(&mut self) {
        // SAFETY: the caller guarantees that all the elements of the cursor are initialized.
        unsafe { self.set_buf_init(true) }
    }

    /// Returns a mutable reference to the whole cursor.
    ///
    /// # Safety
    ///
    /// The caller must not uninitialize any elements of the cursor if it is initialized.
    #[inline]
    pub unsafe fn as_mut(&mut self) -> &mut [MaybeUninit<T>] {
        let filled = self.borrowed_buf().filled;
        // SAFETY: always in bounds
        unsafe { self.buf_mut().get_unchecked_mut(filled..) }
    }

    /// Advances the cursor by asserting that `n` elements have been filled.
    ///
    /// After advancing, the `n` elements are no longer accessible via the cursor and can only be
    /// accessed via the underlying buffer. I.e., the buffer's filled portion grows by `n` elements
    /// and its unfilled portion (and the capacity of this cursor) shrinks by `n` elements.
    ///
    /// If less than `n` elements initialized (by the cursor's point of view), `set_init` should be
    /// called first.
    ///
    /// # Panics
    ///
    /// Panics if there are less than `n` elements initialized.
    #[unstable(feature = "borrowed_buf_init", issue = "160476")]
    #[inline]
    pub fn advance_checked(&mut self, n: usize) -> &mut Self {
        let init_unfilled = if self.borrowed_buf().init { self.capacity() } else { 0 };
        assert!(n <= init_unfilled);

        // SAFETY: the next `n` elements are initialized, as asserted above.
        unsafe { self.advance(n) };
        self
    }

    /// Advances the cursor by asserting that `n` elements have been filled.
    ///
    /// After advancing, the `n` elements are no longer accessible via the cursor and can only be
    /// accessed via the underlying buffer. I.e., the buffer's filled portion grows by `n` elements
    /// and its unfilled portion (and the capacity of this cursor) shrinks by `n` elements.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the first `n` elements of the cursor have been initialized.
    #[inline]
    pub unsafe fn advance(&mut self, n: usize) -> &mut Self {
        // SAFETY: the caller guarantees that the first `n` elements of the cursor are initialized.
        unsafe { self.add_filled(n) };
        self
    }

    /// Append elements to the cursor, advancing position within its buffer.
    ///
    /// # Panics
    ///
    /// Panics if `self.capacity()` is less than `buf.len()`.
    #[inline]
    pub fn append(&mut self, buf: &[T]) {
        assert!(self.capacity() >= buf.len());

        // SAFETY: we do not de-initialize any of the elements of the slice
        unsafe {
            self.as_mut()[..buf.len()].write_copy_of_slice(buf);
        }

        // SAFETY: these elements have just been initialized.
        unsafe { self.advance(buf.len()) };
    }

    /// Runs the given closure with a `BorrowedBuf` containing the unfilled part
    /// of the cursor.
    ///
    /// This enables inspecting what was written to the cursor.
    ///
    /// # Panics
    ///
    /// Panics if the `BorrowedBuf` given to the closure is replaced by another
    /// one.
    pub fn with_unfilled_buf<R>(&mut self, f: impl FnOnce(&mut BorrowedBuf<'_, T>) -> R) -> R {
        let mut buf = BorrowedBuf::from(self.reborrow());
        let prev_ptr = buf.buf() as *const _;
        let res = f(&mut buf);

        // Check that the caller didn't replace the `BorrowedBuf`.
        // This is necessary for the safety of the code below: if the check wasn't
        // there, one could mark some elements as initialized even though they aren't.
        assert!(core::ptr::eq(prev_ptr, buf.buf()));

        let filled = buf.filled;
        let init = buf.init;

        // Update `init` and `filled` fields with what was written to the buffer.
        // `self.buf.filled` was the starting length of the `BorrowedBuf`.
        //
        // SAFETY: These elements were initialized/filled in the `BorrowedBuf`, and therefore they
        // are initialized/filled in the cursor too, because the buffer wasn't replaced.
        unsafe {
            self.set_buf_init(init);
            self.advance(filled);
        }

        res
    }
}

impl<'a, T: Default + Copy> BorrowedCursor<'a, T> {
    /// Initializes all elements in the cursor with their default value and
    /// returns them.
    #[unstable(feature = "borrowed_buf_init", issue = "160476")]
    #[inline]
    pub fn ensure_init(&mut self) -> &mut [T] {
        if !self.borrowed_buf().init {
            // SAFETY: we do not uninitialize any element.
            unsafe { self.as_mut().write_default() };
            // SAFETY: buf is now initialized.
            unsafe { self.set_buf_init(true) };
        }

        // SAFETY: these elements have just been initialized if they weren't before
        unsafe { self.as_mut().assume_init_mut() }
    }
}
