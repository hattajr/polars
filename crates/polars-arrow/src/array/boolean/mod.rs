use either::Either;
use polars_error::{PolarsResult, polars_bail};

use super::{Array, Splitable};
use crate::array::iterator::NonNullValuesIter;
use crate::bitmap::utils::{BitmapIter, ZipValidity};
use crate::bitmap::{Bitmap, MutableBitmap};
use crate::compute::utils::{combine_validities_and, combine_validities_or};
use crate::datatypes::{ArrowDataType, PhysicalType};
use crate::trusted_len::TrustedLen;

mod ffi;
pub(super) mod fmt;
mod from;
mod iterator;
mod mutable;
pub use mutable::*;
mod builder;
pub use builder::*;
#[cfg(feature = "proptest")]
pub mod proptest;

/// A [`BooleanArray`] is Arrow's semantically equivalent of an immutable `Vec<Option<bool>>`.
/// It implements [`Array`].
///
/// One way to think about a [`BooleanArray`] is `(DataType, Arc<Vec<u8>>, Option<Arc<Vec<u8>>>)`
/// where:
/// * the first item is the array's logical type
/// * the second is the immutable values
/// * the third is the immutable validity (whether a value is null or not as a bitmap).
///
/// The size of this struct is `O(1)`, as all data is stored behind an [`std::sync::Arc`].
/// # Example
/// ```
/// use polars_arrow::array::BooleanArray;
/// use polars_arrow::bitmap::Bitmap;
/// use polars_arrow::buffer::Buffer;
///
/// let array = BooleanArray::from([Some(true), None, Some(false)]);
/// assert_eq!(array.value(0), true);
/// assert_eq!(array.iter().collect::<Vec<_>>(), vec![Some(true), None, Some(false)]);
/// assert_eq!(array.values_iter().collect::<Vec<_>>(), vec![true, false, false]);
/// // the underlying representation
/// assert_eq!(array.values(), &Bitmap::from([true, false, false]));
/// assert_eq!(array.validity(), Some(&Bitmap::from([true, false, true])));
///
/// ```
#[derive(Clone)]
pub struct BooleanArray {
    dtype: ArrowDataType,
    values: Bitmap,
    validity: Option<Bitmap>,
}

impl BooleanArray {
    /// The canonical method to create a [`BooleanArray`] out of low-end APIs.
    /// # Errors
    /// This function errors iff:
    /// * The validity is not `None` and its length is different from `values`'s length
    /// * The `dtype`'s [`PhysicalType`] is not equal to [`PhysicalType::Boolean`].
    pub fn try_new(
        dtype: ArrowDataType,
        values: Bitmap,
        validity: Option<Bitmap>,
    ) -> PolarsResult<Self> {
        if validity
            .as_ref()
            .is_some_and(|validity| validity.len() != values.len())
        {
            polars_bail!(ComputeError: "validity mask length must match the number of values")
        }

        if dtype.to_physical_type() != PhysicalType::Boolean {
            polars_bail!(ComputeError: "BooleanArray can only be initialized with a DataType whose physical type is Boolean")
        }

        Ok(Self {
            dtype,
            values,
            validity,
        })
    }

    /// Alias to `Self::try_new().unwrap()`
    pub fn new(dtype: ArrowDataType, values: Bitmap, validity: Option<Bitmap>) -> Self {
        Self::try_new(dtype, values, validity).unwrap()
    }

    /// Returns an iterator over the optional values of this [`BooleanArray`].
    #[inline]
    pub fn iter(&self) -> ZipValidity<bool, BitmapIter<'_>, BitmapIter<'_>> {
        ZipValidity::new_with_validity(self.values().iter(), self.validity())
    }

    /// Returns an iterator over the values of this [`BooleanArray`].
    #[inline]
    pub fn values_iter(&self) -> BitmapIter<'_> {
        self.values().iter()
    }

    /// Returns an iterator of the non-null values.
    #[inline]
    pub fn non_null_values_iter(&self) -> NonNullValuesIter<'_, BooleanArray> {
        NonNullValuesIter::new(self, self.validity())
    }

    /// Returns the length of this array
    #[inline]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// The values [`Bitmap`].
    /// Values on null slots are undetermined (they can be anything).
    #[inline]
    pub fn values(&self) -> &Bitmap {
        &self.values
    }

    /// Returns the optional validity.
    #[inline]
    pub fn validity(&self) -> Option<&Bitmap> {
        self.validity.as_ref()
    }

    /// Returns the arrays' [`ArrowDataType`].
    #[inline]
    pub fn dtype(&self) -> &ArrowDataType {
        &self.dtype
    }

    /// Returns the value at index `i`
    /// # Panic
    /// This function panics iff `i >= self.len()`.
    #[inline]
    pub fn value(&self, i: usize) -> bool {
        self.values.get_bit(i)
    }

    /// Returns the element at index `i` as bool
    ///
    /// # Safety
    /// Caller must be sure that `i < self.len()`
    #[inline]
    pub unsafe fn value_unchecked(&self, i: usize) -> bool {
        self.values.get_bit_unchecked(i)
    }

    /// Returns the element at index `i` or `None` if it is null
    /// # Panics
    /// iff `i >= self.len()`
    #[inline]
    pub fn get(&self, i: usize) -> Option<bool> {
        if !self.is_null(i) {
            // soundness: Array::is_null panics if i >= self.len
            unsafe { Some(self.value_unchecked(i)) }
        } else {
            None
        }
    }

    /// Slices this [`BooleanArray`].
    /// # Implementation
    /// This operation is `O(1)` as it amounts to increase up to two ref counts.
    /// # Panic
    /// This function panics iff `offset + length > self.len()`.
    #[inline]
    pub fn slice(&mut self, offset: usize, length: usize) {
        assert!(
            offset + length <= self.len(),
            "the offset of the new Buffer cannot exceed the existing length"
        );
        unsafe { self.slice_unchecked(offset, length) }
    }

    /// Slices this [`BooleanArray`].
    /// # Implementation
    /// This operation is `O(1)` as it amounts to increase two ref counts.
    ///
    /// # Safety
    /// The caller must ensure that `offset + length <= self.len()`.
    #[inline]
    pub unsafe fn slice_unchecked(&mut self, offset: usize, length: usize) {
        self.validity = self
            .validity
            .take()
            .map(|bitmap| bitmap.sliced_unchecked(offset, length))
            .filter(|bitmap| bitmap.unset_bits() > 0);
        self.values.slice_unchecked(offset, length);
    }

    impl_sliced!();
    impl_mut_validity!();
    impl_into_array!();

    /// Returns a clone of this [`BooleanArray`] with new values.
    /// # Panics
    /// This function panics iff `values.len() != self.len()`.
    #[must_use]
    pub fn with_values(&self, values: Bitmap) -> Self {
        let mut out = self.clone();
        out.set_values(values);
        out
    }

    /// Sets the values of this [`BooleanArray`].
    /// # Panics
    /// This function panics iff `values.len() != self.len()`.
    pub fn set_values(&mut self, values: Bitmap) {
        assert_eq!(
            values.len(),
            self.len(),
            "values length must be equal to this arrays length"
        );
        self.values = values;
    }

    /// Applies a function `f` to the values of this array, cloning the values
    /// iff they are being shared with others
    ///
    /// This is an API to use clone-on-write
    /// # Implementation
    /// This function is `O(f)` if the data is not being shared, and `O(N) + O(f)`
    /// if it is being shared (since it results in a `O(N)` memcopy).
    /// # Panics
    /// This function panics if the function modifies the length of the [`MutableBitmap`].
    pub fn apply_values_mut<F: Fn(&mut MutableBitmap)>(&mut self, f: F) {
        let values = std::mem::take(&mut self.values);
        let mut values = values.make_mut();
        f(&mut values);
        if let Some(validity) = &self.validity {
            assert_eq!(validity.len(), values.len());
        }
        self.values = values.into();
    }

    /// Try to convert this [`BooleanArray`] to a [`MutableBooleanArray`]
    pub fn into_mut(self) -> Either<Self, MutableBooleanArray> {
        use Either::*;

        if let Some(bitmap) = self.validity {
            match bitmap.into_mut() {
                Left(bitmap) => Left(BooleanArray::new(self.dtype, self.values, Some(bitmap))),
                Right(mutable_bitmap) => match self.values.into_mut() {
                    Left(immutable) => Left(BooleanArray::new(
                        self.dtype,
                        immutable,
                        Some(mutable_bitmap.into()),
                    )),
                    Right(mutable) => Right(
                        MutableBooleanArray::try_new(self.dtype, mutable, Some(mutable_bitmap))
                            .unwrap(),
                    ),
                },
            }
        } else {
            match self.values.into_mut() {
                Left(immutable) => Left(BooleanArray::new(self.dtype, immutable, None)),
                Right(mutable) => {
                    Right(MutableBooleanArray::try_new(self.dtype, mutable, None).unwrap())
                },
            }
        }
    }

    /// Returns a new empty [`BooleanArray`].
    pub fn new_empty(dtype: ArrowDataType) -> Self {
        Self::new(dtype, Bitmap::new(), None)
    }

    /// Returns a new [`BooleanArray`] whose all slots are null / `None`.
    pub fn new_null(dtype: ArrowDataType, length: usize) -> Self {
        let bitmap = Bitmap::new_zeroed(length);
        Self::new(dtype, bitmap.clone(), Some(bitmap))
    }

    /// Creates a new [`BooleanArray`] from an [`TrustedLen`] of `bool`.
    #[inline]
    pub fn from_trusted_len_values_iter<I: TrustedLen<Item = bool>>(iterator: I) -> Self {
        MutableBooleanArray::from_trusted_len_values_iter(iterator).into()
    }

    /// Creates a new [`BooleanArray`] from an [`TrustedLen`] of `bool`.
    /// Use this over [`BooleanArray::from_trusted_len_iter`] when the iterator is trusted len
    /// but this crate does not mark it as such.
    ///
    /// # Safety
    /// The iterator must be [`TrustedLen`](https://doc.rust-lang.org/std/iter/trait.TrustedLen.html).
    /// I.e. that `size_hint().1` correctly reports its length.
    #[inline]
    pub unsafe fn from_trusted_len_values_iter_unchecked<I: Iterator<Item = bool>>(
        iterator: I,
    ) -> Self {
        MutableBooleanArray::from_trusted_len_values_iter_unchecked(iterator).into()
    }

    /// Creates a new [`BooleanArray`] from a slice of `bool`.
    #[inline]
    pub fn from_slice<P: AsRef<[bool]>>(slice: P) -> Self {
        MutableBooleanArray::from_slice(slice).into()
    }

    /// Creates a [`BooleanArray`] from an iterator of trusted length.
    /// Use this over [`BooleanArray::from_trusted_len_iter`] when the iterator is trusted len
    /// but this crate does not mark it as such.
    ///
    /// # Safety
    /// The iterator must be [`TrustedLen`](https://doc.rust-lang.org/std/iter/trait.TrustedLen.html).
    /// I.e. that `size_hint().1` correctly reports its length.
    #[inline]
    pub unsafe fn from_trusted_len_iter_unchecked<I, P>(iterator: I) -> Self
    where
        P: std::borrow::Borrow<bool>,
        I: Iterator<Item = Option<P>>,
    {
        MutableBooleanArray::from_trusted_len_iter_unchecked(iterator).into()
    }

    /// Creates a [`BooleanArray`] from a [`TrustedLen`].
    #[inline]
    pub fn from_trusted_len_iter<I, P>(iterator: I) -> Self
    where
        P: std::borrow::Borrow<bool>,
        I: TrustedLen<Item = Option<P>>,
    {
        MutableBooleanArray::from_trusted_len_iter(iterator).into()
    }

    /// Creates a [`BooleanArray`] from an falible iterator of trusted length.
    ///
    /// # Safety
    /// The iterator must be [`TrustedLen`](https://doc.rust-lang.org/std/iter/trait.TrustedLen.html).
    /// I.e. that `size_hint().1` correctly reports its length.
    #[inline]
    pub unsafe fn try_from_trusted_len_iter_unchecked<E, I, P>(iterator: I) -> Result<Self, E>
    where
        P: std::borrow::Borrow<bool>,
        I: Iterator<Item = Result<Option<P>, E>>,
    {
        Ok(MutableBooleanArray::try_from_trusted_len_iter_unchecked(iterator)?.into())
    }

    /// Creates a [`BooleanArray`] from a [`TrustedLen`].
    #[inline]
    pub fn try_from_trusted_len_iter<E, I, P>(iterator: I) -> Result<Self, E>
    where
        P: std::borrow::Borrow<bool>,
        I: TrustedLen<Item = Result<Option<P>, E>>,
    {
        Ok(MutableBooleanArray::try_from_trusted_len_iter(iterator)?.into())
    }

    pub fn true_and_valid(&self) -> Bitmap {
        match &self.validity {
            None => self.values.clone(),
            Some(validity) => combine_validities_and(Some(&self.values), Some(validity)).unwrap(),
        }
    }

    pub fn true_or_valid(&self) -> Bitmap {
        match &self.validity {
            None => self.values.clone(),
            Some(validity) => combine_validities_or(Some(&self.values), Some(validity)).unwrap(),
        }
    }

    /// Returns its internal representation
    #[must_use]
    pub fn into_inner(self) -> (ArrowDataType, Bitmap, Option<Bitmap>) {
        let Self {
            dtype,
            values,
            validity,
        } = self;
        (dtype, values, validity)
    }

    /// Creates a [`BooleanArray`] from its internal representation.
    /// This is the inverted from [`BooleanArray::into_inner`]
    ///
    /// # Safety
    /// Callers must ensure all invariants of this struct are upheld.
    pub unsafe fn from_inner_unchecked(
        dtype: ArrowDataType,
        values: Bitmap,
        validity: Option<Bitmap>,
    ) -> Self {
        Self {
            dtype,
            values,
            validity,
        }
    }
}

impl Array for BooleanArray {
    impl_common_array!();

    fn validity(&self) -> Option<&Bitmap> {
        self.validity.as_ref()
    }

    #[inline]
    fn with_validity(&self, validity: Option<Bitmap>) -> Box<dyn Array> {
        Box::new(self.clone().with_validity(validity))
    }
}

impl Splitable for BooleanArray {
    fn check_bound(&self, offset: usize) -> bool {
        offset <= self.len()
    }

    unsafe fn _split_at_unchecked(&self, offset: usize) -> (Self, Self) {
        let (lhs_values, rhs_values) = unsafe { self.values.split_at_unchecked(offset) };
        let (lhs_validity, rhs_validity) = unsafe { self.validity.split_at_unchecked(offset) };

        (
            Self {
                dtype: self.dtype.clone(),
                values: lhs_values,
                validity: lhs_validity,
            },
            Self {
                dtype: self.dtype.clone(),
                values: rhs_values,
                validity: rhs_validity,
            },
        )
    }
}

impl From<Bitmap> for BooleanArray {
    fn from(values: Bitmap) -> Self {
        Self {
            dtype: ArrowDataType::Boolean,
            values,
            validity: None,
        }
    }
}
