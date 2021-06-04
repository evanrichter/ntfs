// Copyright 2021 Colin Finck <colin@reactos.org>
// SPDX-License-Identifier: GPL-2.0-or-later

use crate::attribute_value::NtfsAttributeValue;
use crate::error::Result;
use crate::structured_values::NewNtfsStructuredValue;
use crate::traits::NtfsReadSeek;
use binread::io::{Read, Seek, SeekFrom};
use binread::{BinRead, BinReaderExt};
use bitflags::bitflags;
use core::iter::FusedIterator;
use core::marker::PhantomData;
use core::mem;

/// Size of all [`IndexEntryHeader`] fields plus some reserved bytes.
const INDEX_ENTRY_HEADER_SIZE: i64 = 16;

#[derive(BinRead, Clone, Debug)]
struct IndexEntryHeader {
    file_ref: u64,
    index_entry_length: u16,
    key_length: u16,
    flags: u8,
}

#[derive(Clone, Debug)]
pub struct NtfsIndexEntry<'n, K>
where
    K: NewNtfsStructuredValue<'n>,
{
    header: IndexEntryHeader,
    value: NtfsAttributeValue<'n>,
    key_type: PhantomData<K>,
}

bitflags! {
    pub struct NtfsIndexEntryFlags: u8 {
        /// This index entry points to a sub-node.
        const HAS_SUBNODE = 0x01;
        /// This is the last index entry in the list.
        const LAST_ENTRY = 0x02;
    }
}

impl<'n, K> NtfsIndexEntry<'n, K>
where
    K: NewNtfsStructuredValue<'n>,
{
    pub(crate) fn new<T>(fs: &mut T, value: NtfsAttributeValue<'n>) -> Result<Self>
    where
        T: Read + Seek,
    {
        let mut value_attached = value.clone().attach(fs);
        let header = value_attached.read_le::<IndexEntryHeader>()?;
        let key_type = PhantomData;

        let entry = Self {
            header,
            value,
            key_type,
        };

        Ok(entry)
    }

    pub fn flags(&self) -> NtfsIndexEntryFlags {
        NtfsIndexEntryFlags::from_bits_truncate(self.header.flags)
    }

    pub fn index_entry_length(&self) -> u16 {
        self.header.index_entry_length
    }

    pub fn key_length(&self) -> u16 {
        self.header.key_length
    }

    /// Returns the structured value of the key of this Index Entry,
    /// or `None` if this Index Entry has no key.
    /// The last Index Entry never has a key.
    pub fn key_structured_value<T>(&self, fs: &mut T) -> Option<Result<K>>
    where
        T: Read + Seek,
    {
        if self.header.key_length == 0 || self.flags().contains(NtfsIndexEntryFlags::LAST_ENTRY) {
            return None;
        }

        let mut value = self.value.clone();
        iter_try!(value.seek(fs, SeekFrom::Current(INDEX_ENTRY_HEADER_SIZE)));
        let length = self.header.key_length as u64;

        let structured_value = iter_try!(K::new(fs, value, length));
        Some(Ok(structured_value))
    }

    /// Returns the Virtual Cluster Number (VCN) of the subnode of this Index Entry,
    /// or `None` if this Index Entry has no subnode.
    pub fn subnode_vcn<T>(&self, fs: &mut T) -> Option<Result<u64>>
    where
        T: Read + Seek,
    {
        if !self.flags().contains(NtfsIndexEntryFlags::HAS_SUBNODE) {
            return None;
        }

        // Read the subnode VCN from the very end of the Index Entry.
        let mut value_attached = self.value.clone().attach(fs);
        iter_try!(value_attached.seek(SeekFrom::Current(
            self.index_entry_length() as i64 - mem::size_of::<u64>() as i64
        )));
        let vcn = iter_try!(value_attached.read_le::<u64>());

        Some(Ok(vcn))
    }
}

#[derive(Clone, Debug)]
pub struct NtfsIndexEntries<'n, K>
where
    K: NewNtfsStructuredValue<'n>,
{
    value: NtfsAttributeValue<'n>,
    end: u64,
    key_type: PhantomData<K>,
}

impl<'n, K> NtfsIndexEntries<'n, K>
where
    K: NewNtfsStructuredValue<'n>,
{
    pub(crate) fn new(value: NtfsAttributeValue<'n>, end: u64) -> Self {
        debug_assert!(end <= value.len());
        let key_type = PhantomData;

        Self {
            value,
            end,
            key_type,
        }
    }

    pub fn attach<'a, T>(self, fs: &'a mut T) -> NtfsIndexEntriesAttached<'n, 'a, K, T>
    where
        T: Read + Seek,
    {
        NtfsIndexEntriesAttached::new(fs, self)
    }

    pub fn next<T>(&mut self, fs: &mut T) -> Option<Result<NtfsIndexEntry<'n, K>>>
    where
        T: Read + Seek,
    {
        if self.value.stream_position() >= self.end {
            return None;
        }

        // Get the current entry.
        let entry = iter_try!(NtfsIndexEntry::new(fs, self.value.clone()));

        if entry.flags().contains(NtfsIndexEntryFlags::LAST_ENTRY) {
            // This is the last entry.
            // Ensure that we don't read any other entries by seeking to the end.
            iter_try!(self.value.seek(fs, SeekFrom::Start(self.end)));
        } else {
            // This is not the last entry.
            // Advance our iterator to the next entry.
            iter_try!(self
                .value
                .seek(fs, SeekFrom::Current(entry.index_entry_length() as i64)));
        }

        Some(Ok(entry))
    }
}

pub struct NtfsIndexEntriesAttached<'n, 'a, K, T>
where
    K: NewNtfsStructuredValue<'n>,
    T: Read + Seek,
{
    fs: &'a mut T,
    index_entries: NtfsIndexEntries<'n, K>,
}

impl<'n, 'a, K, T> NtfsIndexEntriesAttached<'n, 'a, K, T>
where
    K: NewNtfsStructuredValue<'n>,
    T: Read + Seek,
{
    fn new(fs: &'a mut T, index_entries: NtfsIndexEntries<'n, K>) -> Self {
        Self { fs, index_entries }
    }

    pub fn detach(self) -> NtfsIndexEntries<'n, K> {
        self.index_entries
    }
}

impl<'n, 'a, K, T> Iterator for NtfsIndexEntriesAttached<'n, 'a, K, T>
where
    K: NewNtfsStructuredValue<'n>,
    T: Read + Seek,
{
    type Item = Result<NtfsIndexEntry<'n, K>>;

    fn next(&mut self) -> Option<Self::Item> {
        self.index_entries.next(self.fs)
    }
}

impl<'n, 'a, K, T> FusedIterator for NtfsIndexEntriesAttached<'n, 'a, K, T>
where
    K: NewNtfsStructuredValue<'n>,
    T: Read + Seek,
{
}
