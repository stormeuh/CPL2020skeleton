//! File system with inode support + read and write operations on inodes
//!
//! Create a filesystem that has a notion of inodes and blocks, by implementing the [`FileSysSupport`], the [`BlockSupport`] and the [`InodeSupport`] traits together (again, all earlier traits are supertraits of the later ones).
//! Additionally, implement the [`InodeRWSupport`] trait to provide operations to read from and write to inodes
//!
//! [`FileSysSupport`]: ../../cplfs_api/fs/trait.FileSysSupport.html
//! [`BlockSupport`]: ../../cplfs_api/fs/trait.BlockSupport.html
//! [`InodeSupport`]: ../../cplfs_api/fs/trait.InodeSupport.html
//! [`InodeRWSupport`]: ../../cplfs_api/fs/trait.InodeRWSupport.html
//! Make sure this file does not contain any unaddressed `TODO`s anymore when you hand it in.
//!
//! # Status
//!
//! indicate the status of this assignment. If you want to tell something
//! about this assignment to the grader, e.g., you have a bug you can't fix,
//! or you want to explain your approach, write it down after the comments
//! section. If you had no major issues and everything works, there is no need to write any comments.
//!
//! COMPLETED: YES
//!
//! COMMENTS:
//!
//! I realise my implementation violates object encapsulation, but the way buffers are implemented makes it so you'd have to write to an intermediary buffer first. This doubles the amount of writes and thus is undesirable.
//! Implementing methods in buffers for writing buffer to buffer would allow for encapsulation and removes the need for a second copying operation.

use std::cmp::min;
use std::ops::Range;

use crate::b_inode_support::FSName as BFSName;
use std::path::Path;
use thiserror::Error;

use cplfs_api::controller::Device;
use cplfs_api::error_given::APIError;
use cplfs_api::fs::{BlockSupport, FileSysSupport, InodeRWSupport, InodeSupport};
use cplfs_api::types::{Block, Buffer, SuperBlock};
use cplfs_api::types::{FType, InodeLike, DIRECT_POINTERS};

use crate::utils::div_rem;

/// You are free to choose the name for your file system. As we will use
/// automated tests when grading your assignment, indicate here the name of
/// your file system data type so we can just use `FSName` instead of
/// having to manually figure out the name.
pub type FSName = EsFS21;

/// Filesystem supporting reading and writing to inodes
pub struct EsFS21 {
    ifs: BFSName,
}

/// Errors which may be thrown by [`EsFS21`](struct.EsFS21.html).
#[derive(Error, Debug)]
pub enum InodeRWError {
    /// Error while reading from or writing to inode blocks
    #[error("Error performing RW on inode blocks: {0}")]
    RW(&'static str),
    /// Error caused in the lower levels of the filesystem
    #[error("Error in the backing inode supporting FS")]
    Inode(#[from] <BFSName as FileSysSupport>::Error),
    /// Insufficient blocks to perform write
    #[error("Insufficient blocks: {0}")]
    OutOfBlocks(&'static str),
    /// Error caused using function from the API
    #[error("Error caused using function from the API")]
    API(#[from] APIError),
}

impl InodeRWSupport for EsFS21 {
    fn i_read(
        &self,
        inode: &Self::Inode,
        buf: &mut Buffer,
        off: u64,
        n: u64,
    ) -> Result<u64, Self::Error> {
        // calculate how many bytes to read into the buffer
        let bytes_to_read = self.calculate_bytes_to_read(inode.get_size(), buf.len(), off, n)?;
        // calculate the ranges over which blocks must be read into buffer, and first block
        let (ranges_to_read, init_block) =
            self.generate_ranges(bytes_to_read as usize, off as usize);
        // construct iterator starting at init_block
        let mut inode_iter = InodeBlockIterator::new(self, inode, init_block);
        // offset will keep track of where in the buffer we have to write
        let mut buffer_pos = 0;
        for range in ranges_to_read {
            // get the next block to read
            match inode_iter.next() {
                Some(block) => {
                    // obtain reference to relevant slice of data, dictated by the range
                    let block_slice = &block.contents_as_ref()[range];
                    // write into buffer, advance position
                    buf.write_data(block_slice, buffer_pos)?;
                    buffer_pos += block_slice.len() as u64;
                }
                // if no block is returned while we still have ranges to read, return error
                None => {
                    return Err(Self::Error::RW(
                        "ran out of blocks to read, inode size is incorrect",
                    ))
                }
            }
        }
        Ok(bytes_to_read)
    }

    fn i_write(
        &mut self,
        inode: &mut Self::Inode,
        buf: &Buffer,
        off: u64,
        n: u64,
    ) -> Result<(), Self::Error> {
        // perform checks to ensure writing will not cause:
        //   the buffer to run out of data before n bytes have been written
        if buf.len() < n {
            return Err(InodeRWError::RW(
                "buffer is smaller than number of bytes to write.",
            ));
        };
        let sb = self.sup_get().unwrap();
        //   exceeding the block pointer limit
        if DIRECT_POINTERS * sb.block_size < off + n {
            return Err(InodeRWError::OutOfBlocks(
                "not enough blocks can be assigned to inode to write all data.",
            ));
        };
        //   writing at offset would cause a region of empty space in the inode
        if inode.get_size() < off {
            return Err(InodeRWError::RW(
                "write would make inode data non contiguous.",
            ));
        };
        // if n is zero, no work has to be done
        if n == 0 {
            return Ok(());
        }

        // calculate buffer ranges from which to read, plus starting block and initial offset
        let (ranges, skip_blocks, init_off) =
            self.generate_writing_ranges(n as usize, off as usize);
        // create iterators over inode blocks and buffer ranges
        let mut inode_iter = InodeBlockMutIter::new(self, inode, skip_blocks);
        let mut range_iter = ranges.into_iter();
        // get reference to buffer contents, to create slices later
        let buffer_contents = buf.contents_as_ref();
        // write first block, starting at init_off
        // unwrap is safe here because we verified earlier n != 0 and there is a block to write to
        let first_range = range_iter.next().unwrap();
        inode_iter.next().unwrap();
        let buffer_slice = &buffer_contents[first_range];
        inode_iter.curr_b.write_data(buffer_slice, init_off)?;

        // iterate over the remaining ranges, writing them to the inode blocks
        for range in range_iter {
            match inode_iter.next() {
                Some(()) => {
                    let buffer_slice = &buffer_contents[range];
                    inode_iter.curr_b.write_data(buffer_slice, 0)?;
                }
                None => return Err(Self::Error::OutOfBlocks("filesystem may be out of blocks")),
            }
        }
        // write last modified block of iterator to disk
        inode_iter.flush()?;
        // if writing to the inode increased its size, modify on disk
        if inode.get_size() < off + n {
            inode.disk_node.size = off + n;
            self.i_put(&inode)?;
        }
        Ok(())
    }
}

impl EsFS21 {
    /// Calculates how many bytes should be read given an inode, buffer length, offset and requested number of bytes
    /// Number of bytes is the minimum of:
    /// - buffer length
    /// - number of bytes requested
    /// - number of bytes in inode between offset and inode_size
    fn calculate_bytes_to_read(
        &self,
        inode_size: u64,
        buf_len: u64,
        off: u64,
        n: u64,
    ) -> Result<u64, InodeRWError> {
        // calculate how many bytes until the end of the inode is reached
        let (bytes_until_eof, ovf) = inode_size.overflowing_sub(off);
        // if we start reading beyond the end of the inode, return error
        if ovf {
            return Err(InodeRWError::RW("trying to read beyond inode boundaries"));
        };
        // otherwise we will read to whichever comes first: n, the end of the inode or until the buffer is full
        Ok(min(bytes_until_eof, min(buf_len, n)))
    }

    /// Calculates the number of bytes which should be written given an inode, buffer length, offset and requested number of bytes
    /// Number of bytes

    /// calculates the byte ranges in order to read/write n bytes at the given offset
    /// into blocks of size `block_size`
    /// returns a tuple (range, skip_n)
    fn generate_ranges(&self, n: usize, offset: usize) -> (Vec<Range<usize>>, u64) {
        let block_size = self.sup_get().unwrap().block_size as usize;
        let mut ranges = Vec::new();
        // if the number of bytes to read is zero, there is nothing to do
        if n == 0 {
            return (ranges, 0);
        };
        let (skip_n, curr_offset) = div_rem(offset, block_size);
        // subtract curr_off from block_size, should not overflow, but assert as sanity check
        // (eob stands for end of block)
        let (until_eob, ovf) = block_size.overflowing_sub(curr_offset);
        assert!(!ovf);
        // attempt to subtract bytes until end of block from n, overflow indicates we can read all n from the current block
        let (next_n, n_before_eob) = n.overflowing_sub(until_eob);

        // if we can read all n from current block, add that range (starting at curr_offset)
        if n_before_eob {
            ranges.push(curr_offset..(curr_offset + n));
        // otherwise compute ranges needed
        } else {
            // add range from offset until the end of the current block
            ranges.push(curr_offset..block_size);
            let mut curr_n = next_n;
            // once more, subtract block sizes until overflow, indicating curr_n < block_size
            while let (next_n, false) = curr_n.overflowing_sub(block_size) {
                // add range over entire block
                ranges.push(0..block_size);
                curr_n = next_n
            }
            // add range for remaining curr_n
            if curr_n != 0 {
                ranges.push(0..curr_n);
            }
        }
        (ranges, skip_n as u64)
    }

    fn generate_writing_ranges(&self, n: usize, offset: usize) -> (Vec<Range<usize>>, u64, u64) {
        let block_size = self.sup_get().unwrap().block_size as usize;
        let mut ranges = Vec::new();
        // if the number of bytes to read is zero, there is nothing to do
        if n == 0 {
            return (ranges, 0, 0);
        };
        let (skip_blocks, init_off) = div_rem(offset, block_size);
        let (init_n, init_ovf) = block_size.overflowing_sub(init_off);
        assert!(!init_ovf);
        if init_n > n {
            ranges.push(0..n);
        } else {
            ranges.push(0..init_n);
            let mut curr_off = init_n;
            while curr_off + block_size < n {
                ranges.push(curr_off..(curr_off + block_size));
                curr_off += block_size;
            }
            ranges.push(curr_off..n);
        }
        (ranges, skip_blocks as u64, init_off as u64)
    }
}

/// Iterator for iterating over the blocks containing the data of an inode
struct InodeBlockIterator<'a> {
    fs: &'a FSName,
    ino: &'a <FSName as InodeSupport>::Inode,
    i: u64,
}

impl<'a> InodeBlockIterator<'a> {
    fn new(fs: &'a FSName, ino: &'a <FSName as InodeSupport>::Inode, i: u64) -> Self {
        InodeBlockIterator {
            fs: fs,
            ino: ino,
            i: i,
        }
    }
}

impl<'a> Iterator for InodeBlockIterator<'a> {
    type Item = Block;

    // unless the block has index 0, the index is assumed valid
    fn next(&mut self) -> Option<Self::Item> {
        let block_i = self.ino.get_block(self.i);
        if block_i == 0 {
            None
        } else {
            self.i += 1;
            Some(self.fs.b_get(block_i).unwrap())
        }
    }
}

/// Mutable variant of an iterator for blocks belonging to an inode
/// Writes all modifications done
struct InodeBlockMutIter<'a> {
    fs: &'a mut FSName,
    ino: &'a mut <FSName as InodeSupport>::Inode,
    i: u64,
    curr_b: Block,
}

impl<'a> InodeBlockMutIter<'a> {
    fn new(
        fs: &'a mut FSName,
        ino: &'a mut <FSName as InodeSupport>::Inode,
        start_block: u64,
    ) -> Self {
        InodeBlockMutIter {
            fs: fs,
            ino: ino,
            i: start_block,
            curr_b: Block::new_zero(0, 0),
        }
    }

    // flush the cached block back to disk
    fn flush(self) -> Result<(), InodeRWError> {
        Ok(self.fs.b_put(&self.curr_b)?)
    }
}

impl<'a> Iterator for InodeBlockMutIter<'a> {
    type Item = ();

    // get the next block for the inode, and if no more blocks exist, allocate one until the inode references DIRECT_POINTERS blocks
    fn next(&mut self) -> Option<Self::Item> {
        // write back previous block (if previous block does not have index 0, inelegant i know)
        if self.curr_b.block_no != 0 {
            self.fs.b_put(&self.curr_b).ok()?
        };
        // if we are at DIRECT_POINTERS, there are no more blocks to allocate/return
        if self.i == DIRECT_POINTERS {
            return None;
        }
        // get mutable reference to block number
        let mut b_i = self.ino.get_block(self.i);
        // if the block index is zero, allocate a new one
        if b_i == 0 {
            b_i = self.fs.b_alloc().ok()? + self.fs.sup_get().unwrap().datastart;
            self.ino.disk_node.direct_blocks[self.i as usize] = b_i;
        }
        // cache inode block, advance iterator
        self.curr_b = self.fs.b_get(b_i).unwrap();
        self.i += 1;
        Some(())
    }
}

impl FileSysSupport for EsFS21 {
    type Error = InodeRWError;

    fn sb_valid(sb: &SuperBlock) -> bool {
        <BFSName as FileSysSupport>::sb_valid(sb)
    }

    fn mkfs<P: AsRef<Path>>(path: P, sb: &SuperBlock) -> Result<Self, Self::Error> {
        // create new inode fs as backend
        let inode_fs = BFSName::mkfs(path, sb)?;
        // wrap into directory supporting fs
        let new_fs = Self { ifs: inode_fs };
        Ok(new_fs)
    }

    fn mountfs(dev: Device) -> Result<Self, Self::Error> {
        // mount device in inode fs, and wrap it
        let inode_fs = BFSName::mountfs(dev)?;
        Ok(Self { ifs: inode_fs })
    }

    fn unmountfs(self) -> Device {
        self.ifs.unmountfs()
    }
}

// mapping of inode operations onto backing filesystem
impl InodeSupport for EsFS21 {
    type Inode = <BFSName as InodeSupport>::Inode;

    fn i_get(&self, i: u64) -> Result<Self::Inode, Self::Error> {
        Ok(self.ifs.i_get(i)?)
    }

    fn i_put(&mut self, ino: &Self::Inode) -> Result<(), Self::Error> {
        Ok(self.ifs.i_put(ino)?)
    }

    fn i_free(&mut self, i: u64) -> Result<(), Self::Error> {
        Ok(self.ifs.i_free(i)?)
    }

    fn i_alloc(&mut self, ft: FType) -> Result<u64, Self::Error> {
        Ok(self.ifs.i_alloc(ft)?)
    }

    fn i_trunc(&mut self, inode: &mut Self::Inode) -> Result<(), Self::Error> {
        Ok(self.ifs.i_trunc(inode)?)
    }
}

// mapping of block operations onto backing filesystem
impl BlockSupport for EsFS21 {
    fn b_get(&self, i: u64) -> std::result::Result<Block, <Self as FileSysSupport>::Error> {
        Ok(self.ifs.b_get(i)?)
    }
    fn b_put(&mut self, b: &Block) -> std::result::Result<(), <Self as FileSysSupport>::Error> {
        Ok(self.ifs.b_put(b)?)
    }
    fn b_free(&mut self, i: u64) -> std::result::Result<(), <Self as FileSysSupport>::Error> {
        Ok(self.ifs.b_free(i)?)
    }
    fn b_zero(&mut self, i: u64) -> std::result::Result<(), <Self as FileSysSupport>::Error> {
        Ok(self.ifs.b_zero(i)?)
    }
    fn b_alloc(&mut self) -> std::result::Result<u64, <Self as FileSysSupport>::Error> {
        Ok(self.ifs.b_alloc()?)
    }
    fn sup_get(&self) -> std::result::Result<SuperBlock, <Self as FileSysSupport>::Error> {
        Ok(self.ifs.sup_get()?)
    }
    fn sup_put(
        &mut self,
        sb: &SuperBlock,
    ) -> std::result::Result<(), <Self as FileSysSupport>::Error> {
        Ok(self.ifs.sup_put(sb)?)
    }
}

#[cfg(test)]
mod my_tests {}

#[cfg(test)]
#[path = "../../api/fs-tests"]
mod test_with_utils {

    #[path = "utils.rs"]
    mod utils;

    use super::{FSName, InodeBlockMutIter};
    use cplfs_api::fs::{BlockSupport, FileSysSupport, InodeRWSupport, InodeSupport};
    use cplfs_api::types::{Buffer, FType, InodeLike, SuperBlock, DIRECT_POINTERS};
    use std::ops::Range;
    use std::path::PathBuf;

    static BLOCK_SIZE: u64 = 255;
    static NBLOCKS: u64 = 2000;
    static SUPERBLOCK: SuperBlock = SuperBlock {
        block_size: BLOCK_SIZE,
        nblocks: NBLOCKS,
        ninodes: 4,
        inodestart: 1,
        ndatablocks: 1500,
        bmapstart: 3,
        datastart: 4,
    };

    fn disk_prep_path(name: &str) -> PathBuf {
        utils::disk_prep_path(&("fs-images-my-e-".to_string() + name), "img")
    }

    #[test]
    fn test_generate_ranges() {
        let path = disk_prep_path("rrange");
        let fs = FSName::mkfs(&path, &SUPERBLOCK).unwrap();
        let N = 1000;
        let OFFSET = 500;
        let mut calculated_offset = 0;
        let mut calculated_n = 0;
        let (ranges, skip_n) = fs.generate_ranges(N as usize, OFFSET as usize);
        calculated_offset += skip_n * BLOCK_SIZE;
        for r in ranges {
            match r {
                Range { start: 0, end: n } => calculated_n += n,
                Range { start: n1, end: n2 } => {
                    calculated_offset += n1 as u64;
                    let (nn, _) = n2.overflowing_sub(n1);
                    calculated_n += nn;
                }
            }
        }
        assert_eq!(OFFSET, calculated_offset);
        assert_eq!(N, calculated_n);
    }

    #[test]
    fn test_generate_write_ranges() {
        let path = disk_prep_path("wrange");
        let fs = FSName::mkfs(&path, &SUPERBLOCK).unwrap();
        let N: u64 = 1000;
        let OFFSET: u64 = 500;
        let mut calculated_n = 0;
        let (ranges, skip_blocks, init_off) =
            fs.generate_writing_ranges(N as usize, OFFSET as usize);
        let calculated_offset = (skip_blocks * BLOCK_SIZE) + init_off;
        for r in &ranges {
            match r {
                Range { start: 0, end: n } => calculated_n += *n as u64,
                Range { start: n1, end: n2 } => {
                    let (nn, _) = n2.overflowing_sub(*n1);
                    calculated_n += nn as u64;
                }
            }
        }
        for i in 0..ranges.len() - 1 {
            let Range { start: _, end: n12 } = ranges.get(i).unwrap();
            let Range { start: n21, end: _ } = ranges.get(i + 1).unwrap();
            assert!(n12 == n21);
        }
        assert_eq!(OFFSET, calculated_offset);
        assert_eq!(N, calculated_n);
    }

    #[test]
    fn test_mut_inode_iter() {
        let path = disk_prep_path("mut_iter");
        let mut fs = FSName::mkfs(&path, &SUPERBLOCK).unwrap();
        // allocate first block for inode
        fs.b_alloc().unwrap();
        let mut i1 = <<FSName as InodeSupport>::Inode as InodeLike>::new(
            2,
            &FType::TDir,
            0,
            2 * BLOCK_SIZE,
            &[SUPERBLOCK.datastart],
        )
        .unwrap();
        // put inode
        fs.i_put(&i1).unwrap();
        let mut inode_mut_iter = InodeBlockMutIter::new(&mut fs, &mut i1, 0);
        // check we can automatically allocate blocks until DIRECT_POINTERS
        for _ in 0..DIRECT_POINTERS {
            assert!(inode_mut_iter.next().is_some());
        }
        // check we get no more blocks after DIRECT_POINTERS
        assert!(inode_mut_iter.next().is_none());
        // check changes get persisted to disk
        for i in 0..DIRECT_POINTERS {
            assert_eq!(i1.get_block(i), i + SUPERBLOCK.datastart);
        }
    }

    fn generate_some_data() -> Vec<usize> {
        let mut data = Vec::new();
        for i in 0..40 {
            data.push(i * 123);
        }
        data
    }

    static C_BLOCK_SIZE: u64 = 1000;
    static C_NBLOCKS: u64 = 10;
    static C_SUPERBLOCK: SuperBlock = SuperBlock {
        block_size: C_BLOCK_SIZE,
        nblocks: C_NBLOCKS,
        ninodes: 8,
        inodestart: 1,
        ndatablocks: 5,
        bmapstart: 4,
        datastart: 5,
    };

    // erroneous "ran out of blocks to read" error was being thrown while reading directory entries for these parameters
    // problem was caused by function generating ranges while the number of bytes to read is zero
    #[test]
    fn test_c_inspired_read() {
        let path = disk_prep_path("c_read");
        let fs = FSName::mkfs(&path, &C_SUPERBLOCK).unwrap();
        //Get the root node
        let iroot = fs.i_get(1).unwrap();
        let mut buffer = Buffer::new_zero(1386);
        fs.i_read(&iroot, &mut buffer, 0, 1386).unwrap();
    }

    #[test]
    fn test_i_read() {
        let path = disk_prep_path("i_read");
        let mut fs = FSName::mkfs(&path, &SUPERBLOCK).unwrap();
        // allocate first block for inode
        fs.b_alloc().unwrap();
        fs.b_alloc().unwrap();
        let i1 = <<FSName as InodeSupport>::Inode as InodeLike>::new(
            2,
            &FType::TDir,
            0,
            // inode size which goes halfway into the second block
            BLOCK_SIZE + 128,
            &[SUPERBLOCK.datastart, SUPERBLOCK.datastart + 1],
        )
        .unwrap();
        // generate data,
        let some_data = generate_some_data();
        // write manually into blocks
        // len() = 328
        let serialized_data = bincode::serialize(&some_data).unwrap();
        println!("{:?}", serialized_data.len());
        let mut block1 = fs.b_get(SUPERBLOCK.datastart).unwrap();
        for i in 0..BLOCK_SIZE {
            block1
                .serialize_into(&serialized_data[i as usize], i)
                .unwrap();
        }
        fs.b_put(&block1).unwrap();
        let mut block2 = fs.b_get(SUPERBLOCK.datastart + 1).unwrap();
        for i in 0..73 {
            block2
                .serialize_into(&serialized_data[(i + BLOCK_SIZE) as usize], i)
                .unwrap();
        }
        fs.b_put(&block2).unwrap();
        // put inode
        fs.i_put(&i1).unwrap();
        // read data back
        let mut buffer = Buffer::new_zero(328);
        fs.i_read(&i1, &mut buffer, 0, 328).unwrap();
        let read_data = buffer.deserialize_from::<Vec<usize>>(0).unwrap();
        assert_eq!(read_data, some_data);
        // offset past end of file
        assert!(fs
            .i_read(&i1, &mut Buffer::new_zero(20), 2 * BLOCK_SIZE, 10)
            .is_err());
        // read at end of file
        assert_eq!(
            fs.i_read(&i1, &mut Buffer::new_zero(20), BLOCK_SIZE + 128, 10)
                .unwrap(),
            0
        );
        // read more than buffer can contain
        assert_eq!(
            fs.i_read(&i1, &mut Buffer::new_zero(20), 0, 100).unwrap(),
            20
        );
        // read just before end of file
        assert_eq!(
            fs.i_read(&i1, &mut Buffer::new_zero(100), BLOCK_SIZE + 108, 100)
                .unwrap(),
            20
        );
    }

    #[test]
    fn test_i_write() {
        let path = disk_prep_path("i_write");
        let mut fs = FSName::mkfs(&path, &SUPERBLOCK).unwrap();
        let mut i1 = <<FSName as InodeSupport>::Inode as InodeLike>::new(
            2,
            &FType::TDir,
            0,
            // initially the inode contains no data
            0,
            &[0],
        )
        .unwrap();
        // generate data,
        let some_data = generate_some_data();
        println!("{:?}", some_data.len());
        let mut buffer = Buffer::new_zero(328);
        buffer.serialize_into(&some_data, 0).unwrap();
        fs.i_write(&mut i1, &mut buffer, 0, 328).unwrap();
        // execute all scenarios which generate errors, data should not change when read back later
        // start writing beyond end of file
        assert!(fs
            .i_write(&mut i1, &mut Buffer::new_zero(10), 1000, 10)
            .is_err());
        // buffer does not contain as much data as we want to write
        assert!(fs
            .i_write(&mut i1, &mut Buffer::new_zero(0), 0, 10)
            .is_err());
        // writing this many bytes would make inode too large
        assert!(fs
            .i_write(
                &mut i1,
                &mut Buffer::new_zero(0),
                0,
                BLOCK_SIZE * (DIRECT_POINTERS + 1)
            )
            .is_err());
        // check size is changed
        assert_eq!(i1.get_size(), 328);
        // check blocks get allocated
        assert!(i1.get_block(0) != 0);
        assert!(i1.get_block(1) != 0);
        // i_read is tested above and retrieves data correctly, so we trust it
        let mut read_buffer = Buffer::new_zero(328);
        fs.i_read(&mut i1, &mut read_buffer, 0, 328).unwrap();
        assert_eq!(buffer, read_buffer);
    }
}

// WARNING: DO NOT TOUCH THE BELOW CODE -- IT IS REQUIRED FOR TESTING -- YOU WILL LOSE POINTS IF I MANUALLY HAVE TO FIX YOUR TESTS
#[cfg(all(test, any(feature = "e", feature = "all")))]
#[path = "../../api/fs-tests/e_test.rs"]
mod tests;
