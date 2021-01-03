//! File system with block support
//!
//! Create a filesystem that only has a notion of blocks, by implementing the [`FileSysSupport`] and the [`BlockSupport`] traits together (you have no other choice, as the first one is a supertrait of the second).
//!
//! [`FileSysSupport`]: ../../cplfs_api/fs/trait.FileSysSupport.html
//! [`BlockSupport`]: ../../cplfs_api/fs/trait.BlockSupport.html
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
//! If i were to implement the system with bitmapblocks again, I would use global block indexes like was done for InodeSupport in `b_inode_support.rs`
//!

// We import std::error and std::format so we can say error::Error instead of
// std::error::Error, etc.

use std::path::Path;

use cplfs_api::controller::Device;
use cplfs_api::fs::{BlockSupport, FileSysSupport};
use cplfs_api::types::{Block, SuperBlock, DINODE_SIZE, SUPERBLOCK_SIZE};

use math::round::{ceil, floor};
// function calculating integer division, see [`utils.rs`]
use crate::utils::div_rem;
use bitintr::{Blcic, Blcs, Tzcnt};

use thiserror::Error;

/// You are free to choose the name for your file system. As we will use
/// automated tests when grading your assignment, indicate here the name of
/// your file system data type so we can just use `FSName` instead of
/// having to manually figure out your file system name.
pub type FSName = EsFS1;

/// A filesystem supporting operations on blocks of data.
pub struct EsFS1 {
    dev: Device,
    sb: SuperBlock,
}

impl BlockSupport for EsFS1 {
    fn b_get(&self, i: u64) -> Result<Block, Self::Error> {
        let block = self.dev.read_block(i)?;
        Ok(block)
    }

    fn b_put(&mut self, b: &Block) -> Result<(), Self::Error> {
        self.dev.write_block(b)?;
        Ok(())
    }

    fn b_free(&mut self, i: u64) -> Result<(), Self::Error> {
        if self.check_dblock_bounds(i) {
            // get bitmap block containing bit for block
            let mut bitmap_block = BitmapBlock::new_from_data_i(i, self)?;
            // calculate index of block inside bitmap block
            let block_bit_index = i % (self.sb.block_size * 8);
            // set bit to zero (throws error if already free)
            bitmap_block.deallocate(block_bit_index)?;
            // write back to disk
            BitmapBlock::write_back(bitmap_block, self)?;
            Ok(())
        } else {
            Err(Self::Error::Bounds("free"))
        }
    }

    fn b_zero(&mut self, i: u64) -> Result<(), Self::Error> {
        if self.check_dblock_bounds(i) {
            self.b_put(&Block::new_zero(self.sb.datastart + i, self.sb.block_size))
        } else {
            Err(BlockError::Bounds("zero"))
        }
    }

    fn b_alloc(&mut self) -> Result<u64, Self::Error> {
        // iterate over the bitmap blocks
        for (bmb_i, mut bmb) in self.bmap_iter().enumerate() {
            // try to allocate a block
            match bmb.allocate() {
                // if successful, write back to disk.
                Ok(i) => {
                    // when reaching this code block, the loop will not continue so the iterator may
                    // be deallocated, voiding the reference to the filesystem it contains.
                    // this allows mutation of the filesystem in this code block

                    // calculate the block index in the whole data region
                    let global_i = ((bmb_i as u64) * self.sb.block_size * 8) + i;
                    // check if the index is legal
                    if self.check_dblock_bounds(global_i) {
                        // write bitmap change to disk and return
                        bmb.write_back(self)?;
                        // zero the contents of the block on disk
                        // TODO test this
                        self.b_put(&Block::new_zero(
                            global_i + self.sb.datastart,
                            self.sb.block_size,
                        ))?;
                        return Ok(i);
                    } else {
                        // abort the search, throwing an error
                        break;
                    }
                }
                Err(e) => match e {
                    // if an allocation error occurs, go to the next block
                    Self::Error::Allocation(_) => continue,
                    // if another error occurs, this is unexpected so return it
                    _ => return Err(e),
                },
            }
        }
        // throw an error when either all bitmap blocks are full, or we would exceed the
        // number of allowed data blocks in the filesystem
        Err(BlockError::Allocation(
            "no more free blocks in the filesystem",
        ))
    }

    fn sup_get(&self) -> Result<SuperBlock, Self::Error> {
        // superblock is cached so just return that
        Ok(self.sb)
    }

    fn sup_put(&mut self, sup: &SuperBlock) -> Result<(), Self::Error> {
        // copy superblock into filesystem struct
        self.sb = *sup;

        // read first block from disk
        let mut sb_block = self.b_get(0)?;
        // write superblock at beginning of block
        sb_block.serialize_into(sup, 0)?;
        // write back to disk
        self.b_put(&sb_block)?;
        Ok(())
    }
}

impl FileSysSupport for EsFS1 {
    type Error = BlockError;

    /// A superblock is valid when all the following requirements are met:
    /// - It fits in its own block size (as it needs to fit into block 0)
    /// - Its regions are in the correct order:
    /// -- the inode region starts after 0 (to leave space for the superblock)
    /// -- the inode region is before the bitmap region
    /// -- the the bitmap region is before the data region
    /// - The inodes fit into the inode region
    /// - The bitmap region has enough bits to indicate status for all data blocks
    /// - The the data region can fit the required number of blocks
    fn sb_valid(sb: &SuperBlock) -> bool {
        // check if superblock fits in given block size
        let superblock_fits = *SUPERBLOCK_SIZE <= sb.block_size;

        // check inode region comes before bitmap region, idem for bitmap before data
        let region_order_correct =
            0 < sb.inodestart && sb.inodestart < sb.bmapstart && sb.bmapstart < sb.datastart;

        // check inode region is at least as long as required
        let inodes_req_blocks = calc_inode_blocks(sb);
        let inodes_fit = sb.inodestart + inodes_req_blocks <= sb.bmapstart;

        // check if the bitmap region is large enough to keep track of all blocks
        let bitmap_blocks_needed = calc_bitmap_blocks(sb);
        let bitmap_fits = sb.bmapstart + bitmap_blocks_needed <= sb.datastart;

        // check if data region is at least as long as number of blocks
        let data_fits = sb.ndatablocks + sb.datastart <= sb.nblocks;

        // println!("Superblock: {} \n Region order: {} \n Inodes: {} \n Bitmap: {} \n Data: {}", superblock_fits, region_order_correct, inodes_fit, bitmap_fits, data_fits);

        // combine requirements and return
        superblock_fits && region_order_correct && inodes_fit && bitmap_fits && data_fits
    }

    fn mkfs<P: AsRef<Path>>(path: P, sb: &SuperBlock) -> Result<Self, Self::Error> {
        // Check superblock validity
        if !Self::sb_valid(&sb) {
            return Err(BlockError::InitError("Invalid superblock!"));
        }

        // Create filesystem instance
        let mut new_fs = EsFS1 {
            dev: Device::new(path, sb.block_size, sb.nblocks)?,
            sb: *sb,
        };

        new_fs.sup_put(&sb)?;

        // no initialisation for bitmap region needed, as everything is deallocated

        Ok(new_fs)
    }

    fn mountfs(dev: Device) -> Result<Self, Self::Error> {
        // read device superblock and deserialize
        let sbblock = dev.read_block(0)?;
        let sb = sbblock.deserialize_from::<SuperBlock>(0)?;

        // check if superblock is valid
        if !Self::sb_valid(&sb) {
            return Err(BlockError::InitError("Invalid superblock"));
        }

        // check if device parameters match superblock
        sb_match_dev(&dev, &sb)?;

        let new_fs = EsFS1 { dev: dev, sb: sb };

        Ok(new_fs)
    }

    fn unmountfs(self) -> Device {
        // just return the device field
        self.dev
    }
}

impl EsFS1 {
    fn check_dblock_bounds(self: &Self, i: u64) -> bool {
        self.sb.ndatablocks > i
    }

    fn bmap_iter(self: &Self) -> BitmapBlockIterator {
        BitmapBlockIterator::new(self)
    }
}

struct BitmapBlockIterator<'a> {
    fs: &'a FSName,
    i: u64,
}

impl<'a> BitmapBlockIterator<'a> {
    fn new(fs: &FSName) -> BitmapBlockIterator {
        BitmapBlockIterator { fs: fs, i: 0 }
    }
}

impl<'a> Iterator for BitmapBlockIterator<'a> {
    type Item = BitmapBlock;

    fn next(&mut self) -> Option<Self::Item> {
        match BitmapBlock::new(self.i, self.fs) {
            Ok(bmb) => {
                self.i = self.i + 1;
                return Some(bmb);
            }
            Err(e) => match e {
                BlockError::Allocation(_) => return None,
                e => panic!("Problem reading bitmap blocks: {:?}", e),
            },
        };
    }
}

fn calc_bitmap_blocks(sb: &SuperBlock) -> u64 {
    let bitmap_bytes_needed = ceil((sb.ndatablocks as f64) / 8.0, 0);
    ceil(bitmap_bytes_needed / (sb.block_size as f64), 0) as u64
}

fn calc_inode_blocks(sb: &SuperBlock) -> u64 {
    let (inodes_per_block, _) = div_rem(sb.block_size, *DINODE_SIZE);
    ceil((sb.ninodes as f64) / (inodes_per_block as f64), 0) as u64
}

fn sb_match_dev(dev: &Device, sb: &SuperBlock) -> Result<(), BlockError> {
    // check if
    if sb.block_size != dev.block_size {
        return Err(BlockError::InitError(
            "device block size does not match superblock",
        ));
    }
    if sb.nblocks != dev.nblocks {
        return Err(BlockError::InitError(
            "device number of blocks does not match superblock",
        ));
    }
    Ok(())
}

/// Errors which may be thrown by [`EsFS1`](struct.EsFS1.html).
#[derive(Error, Debug)]
pub enum BlockError {
    /// Error during initialisation
    #[error("Error during initialisation of filesystem: {0}")]
    InitError(&'static str),
    /// Wrapping for [`APIError`](../../api/error_given/enum.APIError.html)
    #[error("Error originating in the API")]
    API(#[from] cplfs_api::error_given::APIError),
    /// Block is out of bounds
    #[error("Trying to {0} block outside of filesystem")]
    Bounds(&'static str),
    /// Error while trying to (de)allocate blocks
    #[error("Error during allocation: {0}")]
    Allocation(&'static str),
}

/// Block from inside the bitmap region, on which some useful operations have been defined
struct BitmapBlock {
    b: Block,
}

impl BitmapBlock {
    /// Construct a new bitmap block, checking if it is within boundaries, then
    /// reading it from disk and wrapping it in BitmapBlock struct
    /// The given index is relative to the bitmap region
    fn new(i: u64, fs: &EsFS1) -> Result<BitmapBlock, BlockError> {
        let global_index = fs.sb.bmapstart + i;
        if global_index < fs.sb.datastart {
            let block = BitmapBlock {
                b: fs.b_get(global_index)?,
            };
            Ok(block)
        } else {
            Err(BlockError::Allocation(
                "tried to modify block outside bitmap region",
            ))
        }
    }

    /// Constructs new bitmap block containing the bit indicating the status for
    /// the given block index
    fn new_from_data_i(i: u64, fs: &EsFS1) -> Result<BitmapBlock, BlockError> {
        let bmap_block_i = floor((i as f64) / ((fs.sb.block_size as f64) * 8.0), 0) as u64;
        BitmapBlock::new(bmap_block_i, fs)
    }

    /// Write the block back to the given filesystem, consuming the block in the process
    fn write_back(self, fs: &mut EsFS1) -> Result<(), BlockError> {
        fs.b_put(&self.b)?;
        Ok(())
    }

    /// Sets the ith bit to zero, if it is a one, otherwise gives an error
    fn deallocate(&mut self, i: u64) -> Result<(), BlockError> {
        // calculate in which byte the target bit resides
        let byte_i = floor((i as f64) / 8.0, 0) as u64;
        let mut byte = self.b.deserialize_from::<u8>(byte_i)?;
        // calculate where in the byte the bit is
        let bit_offset = i % 8;
        // create a helper which has a one at the offset
        let helper = 1 << bit_offset as u8;
        // if (byte & helper) are not zero, the target bit is 1 and we can flip it
        if byte & helper != 0 {
            byte = byte ^ helper;
            self.b.serialize_into(&byte, byte_i)?;
            println!("{:b}", self.b.contents_as_ref()[0]);
            Ok(())
        } else {
            Err(BlockError::Allocation(
                "trying to deallocate unallocated block",
            ))
        }
    }

    /// Finds a zero bit and sets it to one, returning its position.
    /// If no zero bits are found, returns an error
    fn allocate(&mut self) -> Result<u64, BlockError> {
        let contents = self.b.contents_as_ref();
        for (i_byte, &byte) in contents.iter().enumerate() {
            // if byte is not max, there is a zero
            if byte != 0b1111_1111 {
                // to get index of lowest 0 bit, isolate it and count trailing zeros
                let i_bit = byte.blcic().tzcnt();
                let zero_bit_i = i_byte as u64 * 8 + i_bit as u64;
                self.b.serialize_into(&byte.blcs(), i_byte as u64)?;
                return Ok(zero_bit_i);
            }
        }
        Err(BlockError::Allocation(
            "no free blocks in this part of the bitmap",
        ))
    }
}

// Here we define a submodule, called `my_tests`, that will contain your unit
// tests for this module.
// You can define more tests in different modules, and change the name of this module
//
// The `test` in the `#[cfg(test)]` annotation ensures that this code is only compiled when we're testing the code.
// To run these tests, run the command `cargo test` in the `solution` directory
//
// To learn more about testing, check the Testing chapter of the Rust
// Book: https://doc.rust-lang.org/book/testing.html
#[cfg(test)]
mod my_tests {

    use super::BitmapBlock;
    use cplfs_api::fs::FileSysSupport;
    use cplfs_api::types::{Block, SuperBlock};

    // weird block size to make sure inode and bitmap calculations have to work with non whole versions
    static BLOCK_SIZE: u64 = 3141;
    static NBLOCKS: u64 = 100000;
    static SB_DATA_TOO_SMALL: SuperBlock = SuperBlock {
        block_size: BLOCK_SIZE,
        // superblock + inodes + bitmap + data
        nblocks: (1 + 5 + 2 + 1),
        ninodes: 10,
        inodestart: 1,
        bmapstart: 6,
        datastart: 8,
        ndatablocks: 4000,
    };

    static SB_BITMAP_TOO_SMALL: SuperBlock = SuperBlock {
        block_size: BLOCK_SIZE,
        nblocks: NBLOCKS,
        ninodes: 10,
        inodestart: 1,
        // 2 blocks needed, 1 allocated for bitmap
        bmapstart: 6,
        datastart: 7,
        ndatablocks: 40000,
    };

    #[test]
    fn sb_valid_data_too_small() {
        assert_eq!(super::FSName::sb_valid(&SB_DATA_TOO_SMALL), false);
    }

    #[test]
    fn sb_valid_bitmap_too_small() {
        assert_eq!(super::FSName::sb_valid(&SB_BITMAP_TOO_SMALL), false);
    }

    #[test]
    fn bmapblock() {
        let mut bmb = BitmapBlock {
            b: Block::new_zero(0, 2),
        };
        // deallocate while nothing has been allocated
        assert!(bmb.deallocate(10).is_err());
        // allocate a bit, check it is the first one in the block
        assert_eq!(bmb.allocate().unwrap(), 0);
        // allocate all blocks
        for _i in 0..15 {
            bmb.allocate().unwrap();
        }
        // check if allocating another block throws an error as expected
        assert!(bmb.allocate().is_err());
        // deallocate three random blocks, check if they get reallocated in order
        bmb.deallocate(3).unwrap();
        bmb.deallocate(5).unwrap();
        bmb.deallocate(12).unwrap();
        assert_eq!(bmb.allocate().unwrap(), 3);
        assert_eq!(bmb.allocate().unwrap(), 5);
        assert_eq!(bmb.allocate().unwrap(), 12);
    }

    #[test]
    fn trivial_unit_test() {
        assert_eq!(2, 2);
        assert!(true);
    }
}

// If you want to write more complicated tests that create actual files on your system, take a look at `utils.rs` in the assignment, and how it is used in the `fs_tests` folder to perform the tests. I have imported it below to show you how it can be used.
// The `utils` folder has a few other useful methods too (nothing too crazy though, you might want to write your own utility functions, or use a testing framework in rust, if you want more advanced features)
#[cfg(test)]
#[path = "../../api/fs-tests"]
mod test_with_utils {

    #[path = "utils.rs"]
    mod utils;

    use super::FSName;
    use cplfs_api::types::SuperBlock;

    use cplfs_api::fs::{BlockSupport, FileSysSupport};
    use std::path::PathBuf;

    static BLOCK_SIZE_ALLOC: u64 = 123;
    static NBLOCKS_ALLOC: u64 = 2000;
    static SUPERBLOCK_ALLOC: SuperBlock = SuperBlock {
        block_size: BLOCK_SIZE_ALLOC,
        nblocks: NBLOCKS_ALLOC,
        ninodes: 0,
        inodestart: 1,
        ndatablocks: 1500,
        bmapstart: 2,
        datastart: 4,
    };

    fn disk_prep_path(name: &str) -> PathBuf {
        utils::disk_prep_path(&("fs-images-my-a-".to_string() + name), "img")
    }

    // extra allocation test which spans over multiple bitmap blocks, where bitmap
    // is shorter than 2 blocks
    #[test]
    fn allocation() {
        let path = disk_prep_path("alloc");
        let mut fs = FSName::mkfs(&path, &SUPERBLOCK_ALLOC).unwrap();
        for _ in 0..SUPERBLOCK_ALLOC.ndatablocks {
            fs.b_alloc().unwrap();
        }
        for bmb in fs.bmap_iter() {
            println!("{:x?}", bmb.b.contents_as_ref());
        }
        assert!(fs.b_alloc().is_err());
    }
}

// Here we define a submodule, called `tests`, that will contain our unit tests
// Take a look at the specified path to figure out which tests your code has to pass.
// As with all other files in the assignment, the testing module for this file is stored in the API crate (this is the reason for the 'path' attribute in the code below)
// The reason I set it up like this is that it allows me to easily add additional tests when grading your projects, without changing any of your files, but you can still run my tests together with yours by specifying the right features (see below) :)
// directory.
//
// To run these tests, run the command `cargo test --features="X"` in the `solution` directory, with "X" a space-separated string of the features you are interested in testing.
//
// WARNING: DO NOT TOUCH THE BELOW CODE -- IT IS REQUIRED FOR TESTING -- YOU WILL LOSE POINTS IF I MANUALLY HAVE TO FIX YOUR TESTS
//The below configuration tag specifies the following things:
// 'cfg' ensures this module is only included in the source if all conditions are met
// 'all' is true iff ALL conditions in the tuple hold
// 'test' is only true when running 'cargo test', not 'cargo build'
// 'any' is true iff SOME condition in the tuple holds
// 'feature = X' ensures that the code is only compiled when the cargo command includes the flag '--features "<some-features>"' and some features includes X.
// I declared the necessary features in Cargo.toml
// (Hint: this hacking using features is not idiomatic behavior, but it allows you to run your own tests without getting errors on mine, for parts that have not been implemented yet)
// The reason for this setup is that you can opt-in to tests, rather than getting errors at compilation time if you have not implemented something.
// The "a" feature will run these tests specifically, and the "all" feature will run all tests.
#[cfg(all(test, any(feature = "a", feature = "all")))]
#[path = "../../api/fs-tests/a_test.rs"]
mod tests;
