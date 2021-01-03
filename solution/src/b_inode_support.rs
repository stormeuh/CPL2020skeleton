//! File system with inode support
//!
//! Create a filesystem that has a notion of inodes and blocks, by implementing the [`FileSysSupport`], the [`BlockSupport`] and the [`InodeSupport`] traits together (again, all earlier traits are supertraits of the later ones).
//!
//! [`FileSysSupport`]: ../../cplfs_api/fs/trait.FileSysSupport.html
//! [`BlockSupport`]: ../../cplfs_api/fs/trait.BlockSupport.html
//! [`InodeSupport`]: ../../cplfs_api/fs/trait.InodeSupport.html
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
//! ...
//!

use crate::a_block_support::FSName as BFSName;
use cplfs_api::controller::Device;
use cplfs_api::fs::{BlockSupport, FileSysSupport, InodeSupport};
use cplfs_api::types::{Block, SuperBlock};
use cplfs_api::types::{DInode, FType, Inode, InodeLike, DINODE_SIZE, DIRECT_POINTERS};
use std::path::Path;
use thiserror::Error;

use crate::utils::div_rem;

/// You are free to choose the name for your file system. As we will use
/// automated tests when grading your assignment, indicate here the name of
/// your file system data type so we can just use `FSName` instead of
/// having to manually figure out the name.
pub type FSName = EsFS2;

/// A filesystem supporting inode and block operations
pub struct EsFS2 {
    /// block filesystem backend
    bfs: BFSName,
}

/// Errors which may be thrown by [`EsFS2`](struct.EsFS2.html).
#[derive(Error, Debug)]
pub enum InodeError {
    /// Error during initialisation of filesystem
    #[error("Error during initialisation of filesystem: {0}")]
    Init(&'static str),
    /// Error caused by going out of bounds on reading or writing inodes
    #[error("Inode index out of bounds")]
    Bounds(),
    /// Error caused by trying to (de)allocate an inode
    #[error("Error during (de)allocation: {0}")]
    Alloc(&'static str),
    /// Error caused when truncating an inode
    #[error("Error truncating inode: {0}")]
    Trunc(&'static str),
    /// Error caused in the block layer
    #[error("Error in the block layer")]
    Block(#[from] <BFSName as FileSysSupport>::Error),
    /// Wrapping for [`APIError`](../../api/error_given/enum.APIError.html)
    #[error("Error originating in the API")]
    API(#[from] cplfs_api::error_given::APIError),
}

// simple mapping of block operation back onto backing filesystem
impl BlockSupport for EsFS2 {
    fn b_get(&self, i: u64) -> std::result::Result<Block, <Self as FileSysSupport>::Error> {
        Ok(self.bfs.b_get(i)?)
    }
    fn b_put(&mut self, b: &Block) -> std::result::Result<(), <Self as FileSysSupport>::Error> {
        Ok(self.bfs.b_put(b)?)
    }
    fn b_free(&mut self, i: u64) -> std::result::Result<(), <Self as FileSysSupport>::Error> {
        Ok(self.bfs.b_free(i)?)
    }
    fn b_zero(&mut self, i: u64) -> std::result::Result<(), <Self as FileSysSupport>::Error> {
        Ok(self.bfs.b_zero(i)?)
    }
    fn b_alloc(&mut self) -> std::result::Result<u64, <Self as FileSysSupport>::Error> {
        Ok(self.bfs.b_alloc()?)
    }
    fn sup_get(&self) -> std::result::Result<SuperBlock, <Self as FileSysSupport>::Error> {
        Ok(self.bfs.sup_get()?)
    }
    fn sup_put(
        &mut self,
        sb: &SuperBlock,
    ) -> std::result::Result<(), <Self as FileSysSupport>::Error> {
        Ok(self.bfs.sup_put(sb)?)
    }
}

impl InodeSupport for EsFS2 {
    type Inode = Inode;

    fn i_get(&self, i: u64) -> Result<Self::Inode, Self::Error> {
        let inode_block = InodeBlock::new_from_inode_i(i, self)?;
        inode_block.i_get(i)
    }

    fn i_put(&mut self, ino: &Self::Inode) -> Result<(), Self::Error> {
        let mut inode_block = InodeBlock::new_from_inode_i(ino.get_inum(), self)?;
        inode_block.i_put(ino)?;
        inode_block.write_back(self)
    }

    fn i_free(&mut self, i: u64) -> Result<(), Self::Error> {
        // read inode containing block and inode from disk
        let mut inode_block = InodeBlock::new_from_inode_i(i, self)?;
        let mut inode = inode_block.i_get(i)?;

        // check if nlink is 0, otherwise do nothing
        if inode.get_nlink() > 0 {
            return Ok(());
        };
        // check if inode hasn't been freed already
        if inode.get_ft() == FType::TFree {
            return Err(Self::Error::Alloc(
                "trying to free inode which is already free.",
            ));
        };

        // deallocate all valid blocks pointed to by direct pointer list
        self.deallocate_valid_blocks(inode.get_size(), &inode.disk_node.direct_blocks)?;

        // change inode fields to reflect current status, write back to disk
        inode.disk_node.ft = FType::TFree;
        inode.disk_node.direct_blocks = [0; DIRECT_POINTERS as usize];
        inode_block.i_put(&inode)?;
        inode_block.write_back(self)
    }

    fn i_alloc(&mut self, ft: FType) -> Result<u64, Self::Error> {
        // create inode iterator, to traverse inodes on disk
        let mut inode_iter = self.inode_iter();
        // look at inodes until free one is found
        while let Some(inode) = inode_iter.next() {
            if inode.get_ft().eq(&FType::TFree) {
                break;
            };
        }
        // get the current inode and block, consuming the iterator in the process
        match inode_iter.curr() {
            Some((mut inode, mut inode_block)) => {
                inode.disk_node.ft = ft;
                inode.disk_node.direct_blocks = [0; DIRECT_POINTERS as usize];
                inode.disk_node.size = 0;
                inode_block.i_put(&inode)?;
                inode_block.write_back(self)?;
                return Ok(inode.get_inum());
            }
            None => return Err(Self::Error::Alloc("no more free inodes in system")),
        }
    }

    fn i_trunc(&mut self, inode: &mut Self::Inode) -> Result<(), Self::Error> {
        let i = inode.get_inum();
        let mut inode_block = InodeBlock::new_from_inode_i(i, self)?;
        // sanity check, see if inode with given number corresponds to what is on disk
        if !inode_block.i_get(i)?.eq(inode) {
            return Err(Self::Error::Trunc(
                "given inode contents do not correspond to what is on disk!",
            ));
        };

        print!("Inode: {} Blocks: ", i);
        // deallocate blocks pointed to by direct pointer list
        self.deallocate_valid_blocks(inode.get_size(), &inode.disk_node.direct_blocks)?;

        // reset block pointers to zero, set size to 0
        inode.disk_node.direct_blocks = [0; DIRECT_POINTERS as usize];
        inode.disk_node.size = 0;
        // write back to disk
        inode_block.i_put(&inode)?;
        inode_block.write_back(self)
    }
}

impl FileSysSupport for EsFS2 {
    type Error = InodeError;

    /// a superblock is as valid in the block layer as in the inode layer
    /// see [`a_block_support::FSName`](../a_block_support/type.FSName.hmtl)
    fn sb_valid(sb: &SuperBlock) -> bool {
        BFSName::sb_valid(sb)
    }

    fn mkfs<P: AsRef<Path>>(path: P, sb: &SuperBlock) -> Result<Self, Self::Error> {
        // create new block filesystem as backend
        let block_fs = BFSName::mkfs(path, sb)?;
        let mut new_fs = Self { bfs: block_fs };
        new_fs.init_inode_region()?;
        Ok(new_fs)
    }

    fn mountfs(dev: Device) -> Result<Self, Self::Error> {
        // mount device in block layer
        let block_fs = BFSName::mountfs(dev)?;
        // wrap block fs into inode supporting fs
        let new_fs = Self { bfs: block_fs };
        Ok(new_fs)
    }

    fn unmountfs(self) -> Device {
        self.bfs.unmountfs()
    }
}

impl EsFS2 {
    fn inode_iter(&self) -> InodeIterator {
        InodeIterator::new(self)
    }

    // initialise the inode region by writing
    fn init_inode_region(&mut self) -> Result<(), <Self as FileSysSupport>::Error> {
        // write an empty inode to every spot
        for i in 0..(self.sup_get()?.ninodes) {
            let inode = <Inode as InodeLike>::new(i, &FType::TFree, 0, 0, &[]);
            // while this error can only occur when something is wrong with the implementation,
            // it's still cleaner to throw an error than to straight panic
            if inode.is_none() {
                return Err(InodeError::Init("can't init inode region"));
            }
            self.i_put(&inode.unwrap())?;
        }
        Ok(())
    }

    /// Deallocates "valid" blocks pointed to by the list of blocks given
    /// ie deallocates the first size/block_size blocks from bs
    fn deallocate_valid_blocks(
        &mut self,
        size: u64,
        bs: &[u64],
    ) -> Result<(), <Self as FileSysSupport>::Error> {
        // get superblock to use later
        let sb = self.sup_get().unwrap();

        // use a vector to store blocks to be freed, so we can sort it later
        let mut b_to_free = Vec::new();
        let mut block_end_byte = 0;
        for &b_i in bs {
            if block_end_byte < size {
                // convert block indices from relative to disk to relative to data region
                let (b_i_rel, ovf) = b_i.overflowing_sub(sb.datastart);
                // if no overflow has happened during the subtraction, we are in the data region
                if !ovf {
                    b_to_free.push(b_i_rel);
                }
                block_end_byte += sb.block_size;
            } else {
                break;
            }
        }
        // sort the vector to take advantage of bitmap caching
        b_to_free.sort();
        for b_i in b_to_free {
            self.b_free(b_i)?;
        }
        Ok(())
    }
}

/// Iterator for inodes of [`EsFS2`](struct.EsFS2.html).
pub struct InodeIterator<'a> {
    fs: &'a FSName,
    i: u64,
    ib: InodeBlock,
}

impl<'a> InodeIterator<'a> {
    fn new(fs: &'a EsFS2) -> Self {
        InodeIterator {
            fs: fs,
            // init inode to 0, because when next() gets called it immediately increments the counter
            i: 0,
            ib: InodeBlock::new_from_inode_i(1, fs).unwrap(),
        }
    }

    /// Iterator destructor returning the current inode and its containing block
    fn curr(self) -> Option<(Inode, InodeBlock)> {
        // try to get the current inode, if successful return along with block
        match self.ib.i_get(self.i) {
            Ok(inode) => Some((inode, self.ib)),
            Err(_) => None,
        }
    }
}

impl<'a> Iterator for InodeIterator<'a> {
    type Item = Inode;

    /// Gets the next element
    fn next(&mut self) -> Option<Self::Item> {
        self.i = self.i + 1;
        // try to get the next element from the current cached block
        match self.ib.i_get(self.i) {
            // if it succeeds, increment counter and return
            Ok(inode) => Some(inode),
            // if it fails, either the inode is in the next block, or it does not exist
            Err(_) => {
                // construct new inode block which should contain the required inode
                match InodeBlock::new_from_inode_i(self.i, self.fs) {
                    // if construction succeeds, cache InodeBlock and return the inode
                    Ok(ib) => {
                        self.ib = ib;
                        let inode = self.ib.i_get(self.i).unwrap();
                        Some(inode)
                    }
                    // if it fails, there are no more inodes
                    Err(_) => None,
                }
            }
        }
    }
}

/// Block from inside the inode region, on which inode manipulation operations have been defined
struct InodeBlock {
    /// Backing block containing the actual data
    b: Block,
    /// lowest inode index contained in this block
    il: u64,
    /// highest inode index contained in this block
    ih: u64,
}

impl InodeBlock {
    /// Create a new inode block, which contains the given inode index i
    fn new_from_inode_i(i: u64, fs: &FSName) -> Result<Self, InodeError> {
        // get superblock from fs
        let sb = fs.sup_get().unwrap();
        // check if inode index is valid
        if i >= sb.ninodes && i != 0 {
            return Err(InodeError::Bounds());
        };
        // calculate the number of inodes per block, and in which block inode i resides
        let (inodes_per_block, _) = div_rem(sb.block_size, *DINODE_SIZE);
        let (inode_block_num, _) = div_rem(i, inodes_per_block);
        // read that block from disk
        let block = fs.b_get(sb.inodestart + inode_block_num)?;
        // construct new InodeBlock
        let new_ib = InodeBlock {
            b: block,
            il: inode_block_num * inodes_per_block,
            ih: (inode_block_num + 1) * inodes_per_block - 1,
        };
        Ok(new_ib)
    }

    /// Get inode from the given index
    fn i_get(&self, i: u64) -> Result<Inode, InodeError> {
        // calculate the byte offset in the block
        let off = self.calc_off(i)?;
        // deserialize disk inode from block
        let dino = self.b.deserialize_from::<DInode>(off)?;
        // wrap into inode and return
        Ok(Inode::new(i, dino))
    }

    /// Write a given inode into the block
    fn i_put(&mut self, inode: &Inode) -> Result<(), InodeError> {
        // calculate byte offset in the block
        let off = self.calc_off(inode.inum)?;
        // serialize disk inode into block
        self.b.serialize_into(&inode.disk_node, off)?;
        Ok(())
    }

    /// Calculate the byte offset for the given inode index
    /// errors out if the inode is not in this block
    fn calc_off(&self, i: u64) -> Result<u64, InodeError> {
        // subtract the inode index from the lowest inode index in the block, to get the relative index
        let (rel_i, ovf) = i.overflowing_sub(self.il);
        // if the subtraction overflowed, or the index is larger than the max index, throw bounds error
        if i > self.ih || ovf {
            return Err(InodeError::Bounds());
        };
        // calculate the byte offset in the block
        Ok(rel_i * *DINODE_SIZE)
    }

    /// Write the block backing this struct back to disk, consuming the entire
    /// struct in the process
    /// Does nothing if the block has not been modified
    fn write_back(self, fs: &mut FSName) -> Result<(), InodeError> {
        fs.b_put(&self.b)?;
        Ok(())
    }
}

#[cfg(test)]
mod my_tests {}

#[cfg(test)]
#[path = "../../api/fs-tests"]
mod test_with_utils {

    #[path = "utils.rs"]
    mod utils;

    use super::FSName;
    use std::path::PathBuf;

    use cplfs_api::fs::{BlockSupport, FileSysSupport, InodeSupport};
    use cplfs_api::types::{DInode, FType, InodeLike, SuperBlock, DINODE_SIZE};

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
        utils::disk_prep_path(&("fs-images-my-b-".to_string() + name), "img")
    }

    #[test]
    fn inodes_over_multiple_blocks() {
        let path = disk_prep_path("mul_blocks");
        let mut my_fs = FSName::mkfs(&path, &SUPERBLOCK).unwrap();

        let i1 = <<FSName as InodeSupport>::Inode as InodeLike>::new(1, &FType::TFile, 0, 0, &[])
            .unwrap();
        let i2 = <<FSName as InodeSupport>::Inode as InodeLike>::new(2, &FType::TDir, 1, 0, &[])
            .unwrap();
        let i3 = <<FSName as InodeSupport>::Inode as InodeLike>::new(3, &FType::TFile, 2, 0, &[])
            .unwrap();
        let i4 = <<FSName as InodeSupport>::Inode as InodeLike>::new(4, &FType::TFile, 2, 0, &[])
            .unwrap();
        my_fs.i_put(&i1).unwrap();
        my_fs.i_put(&i2).unwrap();
        my_fs.i_put(&i3).unwrap();
        // assert no more than 4 inodes can be written
        assert!(my_fs.i_put(&i4).is_err());
        // manually read back from disk
        let inodeb1 = my_fs.b_get(SUPERBLOCK.inodestart).unwrap();
        let inodeb2 = my_fs.b_get(SUPERBLOCK.inodestart + 1).unwrap();
        let i1_read = inodeb1.deserialize_from::<DInode>(*DINODE_SIZE).unwrap();
        assert_eq!(i1_read, i1.disk_node);
        let i2_read = inodeb2.deserialize_from::<DInode>(0).unwrap();
        assert_eq!(i2_read, i2.disk_node);
        let i3_read = inodeb2.deserialize_from::<DInode>(*DINODE_SIZE).unwrap();
        assert_eq!(i3_read, i3.disk_node);
    }
}

// WARNING: DO NOT TOUCH THE BELOW CODE -- IT IS REQUIRED FOR TESTING -- YOU WILL LOSE POINTS IF I MANUALLY HAVE TO FIX YOUR TESTS
#[cfg(all(test, any(feature = "b", feature = "all")))]
#[path = "../../api/fs-tests/b_test.rs"]
mod tests;
