//! File system with directory support
//!
//! Create a filesystem that has a notion of blocks, inodes and directory inodes, by implementing the [`FileSysSupport`], the [`BlockSupport`], the [`InodeSupport`] and the [`DirectorySupport`] traits together (again, all earlier traits are supertraits of the later ones).
//!
//! [`FileSysSupport`]: ../../cplfs_api/fs/trait.FileSysSupport.html
//! [`BlockSupport`]: ../../cplfs_api/fs/trait.BlockSupport.html
//! [`InodeSupport`]: ../../cplfs_api/fs/trait.InodeSupport.html
//! [`DirectorySupport`]: ../../cplfs_api/fs/trait.DirectorySupport.html
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
//! Having to use dirlookup in dirlink is terrible, as it iterates over the inode's direntries twice: once to look for an entry with the same name, and once to find a free one.
//! The way creating directory entries works is in my opinion not expressive enough, as it is impossible to communicate what is wrong with the given directory name to the user, except that it is bad.
//!

// BFS for Backing FileSystem
use crate::e_inode_RW_support::FSName as BFSName;
use thiserror::Error;
use std::path::Path;
use std::collections::VecDeque;
use lazy_static::lazy_static;
use regex::Regex;
use crate::utils::{div_rem};

use cplfs_api::fs::{FileSysSupport, BlockSupport, InodeSupport, InodeRWSupport, DirectorySupport};
use cplfs_api::types::{DirEntry, DIRNAME_SIZE, DIRENTRY_SIZE};
use cplfs_api::types::{Inode, DInode, InodeLike, FType, DIRECT_POINTERS};
use cplfs_api::types::{SuperBlock, Block, Buffer};
use cplfs_api::controller::{Device};
use cplfs_api::error_given::{APIError};


/// You are free to choose the name for your file system. As we will use
/// automated tests when grading your assignment, indicate here the name of
/// your file system data type so we can just use `FSName` instead of
/// having to manually figure out the name.
pub type FSName = EsFS3;

/// A filesystem with directory support
pub struct EsFS3 {
	ifs: BFSName,
}

static INIT_ROOT_INODE: Inode = Inode {
	inum: 1, 
	disk_node: DInode {
		ft: FType::TDir, 
		nlink: 1,
		size: 0,
		direct_blocks: [0; DIRECT_POINTERS as usize],
	},
};

lazy_static!{
	/// Regex string recognizing all valid directory entry names (for building larger regexes which recognise paths)
	pub static ref RE_DIRNAME_INNER: Regex = Regex::new(format!("(([A-Za-z0-9]{{1,{}}})|(\\.{{1,2}}))",DIRNAME_SIZE).as_str()).unwrap();
	/// Regex recognizing all valid directory entry names
	pub static ref RE_DIRNAME: Regex = Regex::new(format!("^{}$",*RE_DIRNAME_INNER).as_str()).unwrap();
}


/// Errors which may be thrown by [`EsFS3`](struct.EsFS3.html).
#[derive(Error, Debug)]
pub enum DirectoryError {
	/// Error during initialisation of filesystem
	#[error("Error during initialisation of filesystem: {0}")]
	Init(&'static str),
	/// Error thrown when lookup in the directory did not yield a result
	#[error("Item not found inside this directory")]
	ItemNotFound,
	/// Error thrown when trying to perform lookup in inode which is not a directory
	#[error("Given Inode is faulty: {0}")]
	IllegalInode(&'static str),
	/// Something is wrong with the direntry we want to create
	#[error("Given DirEntry is faulty: {0}")]
	IllegalDirEntry(&'static str),
	/// Error caused in the lower filesystem layers
	#[error("Error in the lower layers")]
	Backend(#[from] <BFSName as FileSysSupport>::Error),
	/// Error caused using function from the API
	#[error("Error caused using function from the API")]
	API(#[from] APIError),
}

impl DirectorySupport for EsFS3 {
	fn new_de(inum: u64, name: &str) -> Option<DirEntry> { 
		let mut de = DirEntry {
			inum: inum,
			name: ['\0'; DIRNAME_SIZE],
		};
		Self::set_name_str(&mut de, name)?;
		Some(de)
	}
	fn get_name_str(de: &DirEntry) -> String { 
		let mut name = String::with_capacity(DIRNAME_SIZE);
		for &c in &de.name {
			if c == '\0' {break;}
			name.push(c);
		}
		name
	}
	fn set_name_str(de: &mut DirEntry, name: &str) -> Option<()> { 
		if RE_DIRNAME.is_match(name) {
			for (i,c) in name.chars().enumerate() {
				de.name[i] = c;
			}
			Some(())
		} else {
			None
		}
	}
	
	fn dirlookup(&self, inode: &Self::Inode, name: &str) -> Result<(Self::Inode, u64), Self::Error> { 
		// if the given inode is not a directory, return an error
		if inode.get_ft() != FType::TDir { return Err(DirectoryError::IllegalInode("not a directory")) }
		// check the given inode is consistent with the one on disk
		if !self.i_get(inode.get_inum())?.eq(inode) { return Err(DirectoryError::IllegalInode("inconsistency on disk"))}
		// otherwise construct an iterator over the directory entries of this inode
		let direntry_iterator = DirEntryIterator::new(self, inode)?;
		for (de_i, de) in direntry_iterator.enumerate() {
			// iterate until element is found with equal name
			if Self::get_name_str(&de).as_str() == name {
				// direntries are written contiguously, so offset is just number * size
				let offset = (de_i as u64) * *DIRENTRY_SIZE;
				// retrieve inode from disk
				let de_inode = self.i_get(de.inum)?;
				return Ok((de_inode, offset))
			}
		}
		// or until the directory entries are exhausted, in which case the item cannot be found
		Err(DirectoryError::ItemNotFound)
	}
	
	fn dirlink(&mut self, inode: &mut Self::Inode, name: &str, inum: u64) -> Result<u64, Self::Error> { 
		// create directory entry, throwing an error if its name is invalid
		let de = match Self::new_de(inum, name) {
			Some(de) => de,
			None => return Err(DirectoryError::IllegalDirEntry("invalid name")),
		};
		// looks up the inode referenced by inum, throws error if its is of TFree
		if self.i_get(inum)?.get_ft() == FType::TFree { return Err(DirectoryError::IllegalDirEntry("Inode referenced by inum is not in use"))};
		// lookup name in given inode, throw errors where necessary
		match self.dirlookup(inode, name) {
			// if an item is found, 
			Ok(_) => return Err(DirectoryError::IllegalDirEntry("already exists in inode")),
			Err(e) => match e {
				// on ItemNotFound, continue as this is what we want
				DirectoryError::ItemNotFound => {},
				// if another error occurs, return it (includes the case where inode does not match what is on disk, is checked by dirlookup)
				_ => return Err(e),
			},
		}
		// no errors, all set, create buffer and write directory entry into it
		let mut buf = Buffer::new_zero(*DIRENTRY_SIZE);
		buf.serialize_into(&de, 0)?;
		// set offset to inode size, if no empty dir entry is found, this one is used
		let mut offset = inode.get_size();
		// create entry iterator and start looking for empty one
		let direntry_iterator = DirEntryIterator::new(self, inode)?;
		for (de_i, de) in direntry_iterator.enumerate() {
			// empty directory entry found, write there
			if de.inum == 0 {
				// set offset to current directory entry offset
				offset = (de_i as u64) * *DIRENTRY_SIZE;
				break;
			}
		}
		// in case no empty directory entry is found, add one to the end (so offset is the *current* size of the inode)
		self.i_write(inode, &buf, offset, *DIRENTRY_SIZE)?;
		// increase number of references and write to disk
		// unless the entry is a self reference
		if inum == 5 {print!("{} -> 5: {} ", inode.get_inum(), name)};
		if inode.get_inum() != inum {
			if inum == 5 {println!("linking")};
			let mut inum_inode = self.i_get(inum)?;
			inum_inode.disk_node.nlink += 1;
			self.i_put(&inum_inode)?;
		};
		Ok(offset)
	}
}

/// Iterator for iterating over the directory entries in inodes
pub struct DirEntryIterator<'a> {
	fs: &'a FSName,
	inode: &'a <FSName as InodeSupport>::Inode,
	d_cache: VecDeque<DirEntry>,
	last_de: u64,
}

impl<'a> DirEntryIterator<'a> {
	/// create a new DirEntryIterator, iterating over the entries of inode in filesystem fs
	/// Errors if the given inode does not represent a directory
	pub fn new(fs: &'a FSName, inode: &'a <FSName as InodeSupport>::Inode) -> Result<Self, DirectoryError> {
		if inode.get_ft() != FType::TDir {return Err(DirectoryError::IllegalInode("not a directory, cannot iterate over it"))};
		let (n_direntry, _) = div_rem(fs.sup_get().unwrap().block_size, *DIRENTRY_SIZE);
		Ok(DirEntryIterator {
			fs: fs,
			inode: inode,
			// initialise direntry cache to hold one more direntry than fit evenly into an inode block, should ensure blocks get read at most 2 times
			d_cache: VecDeque::with_capacity(1 + n_direntry as usize),
			last_de: 0,
		})
	}
}

impl<'a> Iterator for DirEntryIterator<'a> {
	type Item = DirEntry;

	fn next(&mut self) -> Option<Self::Item> {
		// try to get cached direntry, otherwise read from disk
		match self.d_cache.pop_front() {
			Some(de) => return Some(de),
			None => {
				// calculate where we last read direntries in the block
				let offset = self.last_de * *DIRENTRY_SIZE;
				// initialise new buffer to hold as many direntries as the cache can hold
				let bytes_to_read = self.d_cache.capacity() as u64 * *DIRENTRY_SIZE;
				let mut buffer = Buffer::new_zero(bytes_to_read);
				let bytes_read = self.fs.i_read(self.inode, &mut buffer, offset, bytes_to_read).unwrap();
				// calculate number of direntries read, (asserting that the remainder is zero would be a good sanity check, but fails in test due to incorrect inode size)
				let (direntries_read, _) = div_rem(bytes_read, *DIRENTRY_SIZE);
				// update last_de to the last direntry read
				self.last_de += direntries_read;
				// deserialize direntries from buffer, and put them at the back of the cache
				for direntry_i in 0..direntries_read {
					let direntry = buffer.deserialize_from::<DirEntry>(direntry_i * *DIRENTRY_SIZE).unwrap();
					self.d_cache.push_back(direntry);
				}
				// return direntry at the front of the cache
				return self.d_cache.pop_front()
			}
		}
	}

}

impl FileSysSupport for EsFS3 {
	type Error = DirectoryError;

	fn sb_valid(sb: &SuperBlock) -> bool {
		<BFSName as FileSysSupport>::sb_valid(sb)
	}

	fn mkfs<P: AsRef<Path>>(path: P, sb: &SuperBlock) -> Result<Self, Self::Error> {
		// create new inode fs as backend
		let inode_fs = BFSName::mkfs(path, sb)?;
		// wrap into directory supporting fs
		let mut new_fs = EsFS3 {
			ifs: inode_fs,
		};
		// write root directory inode
		new_fs.i_put(&INIT_ROOT_INODE)?;
		Ok(new_fs)
	}

	fn mountfs(dev: Device) -> Result<Self, Self::Error> {
		// mount device in inode fs, and wrap it
		let inode_fs = BFSName::mountfs(dev)?;
		Ok(Self {ifs: inode_fs,})
	}

	fn unmountfs(self) -> Device {
		self.ifs.unmountfs()
	}
}

// mapping of inode rw operations onto backing filesystem
impl InodeRWSupport for EsFS3 {
	fn i_read(&self, inode: &Self::Inode, buf: &mut Buffer, off: u64, n: u64) -> Result<u64, Self::Error> {
		Ok(self.ifs.i_read(inode, buf, off, n)?)
	}

	fn i_write(&mut self, inode: &mut Self::Inode, buf: &Buffer, off: u64, n: u64) -> Result<(), Self::Error> {
		Ok(self.ifs.i_write(inode, buf, off, n)?)
	}
}

// mapping of inode operations onto backing filesystem
impl InodeSupport for EsFS3 {
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
impl BlockSupport for EsFS3 {
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
	fn sup_put(&mut self, sb: &SuperBlock) -> std::result::Result<(), <Self as FileSysSupport>::Error> { 
		Ok(self.ifs.sup_put(sb)?)
	}
}

#[cfg(test)]
mod my_tests {
	use super::RE_DIRNAME;
	use super::FSName;
	use cplfs_api::fs::DirectorySupport;

	#[test]
	fn test_dirname_regex(){
		assert!(RE_DIRNAME.is_match("test"));
		assert!(RE_DIRNAME.is_match("testing123"));
		assert!(RE_DIRNAME.is_match("verylongnameee"));
		assert!(!RE_DIRNAME.is_match("verylongnameeee"));
		assert!(RE_DIRNAME.is_match("."));
		assert!(RE_DIRNAME.is_match(".."));
		assert!(!RE_DIRNAME.is_match("exceedinglylongname"));
		assert!(!RE_DIRNAME.is_match("..."));
		assert!(!RE_DIRNAME.is_match("namewith."));
		assert!(!RE_DIRNAME.is_match(""));
	}

	#[test]
	fn test_new_de() {
		// verify name error is actually passed on through new_de
		assert!(!FSName::new_de(1, "root").is_none());
		assert!(FSName::new_de(1, "exceedinglylongname").is_none());
	}
}

#[cfg(test)]
#[path = "../../api/fs-tests"]
mod test_with_utils {

	#[path = "utils.rs"]
    mod utils;
	
	use super::FSName;
	use std::path::PathBuf;
	
	use cplfs_api::types::{SuperBlock, Buffer, InodeLike, FType, DIRENTRY_SIZE};
	use cplfs_api::fs::{FileSysSupport, BlockSupport, InodeSupport, DirectorySupport, InodeRWSupport};

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
        utils::disk_prep_path(&("fs-images-my-c-".to_string() + name), "img")
    }

	#[test]
	fn test_dirlookup() {
		let path = disk_prep_path("dirlookup");
		let mut fs = FSName::mkfs(&path, &SUPERBLOCK).unwrap();
		fs.b_alloc().unwrap();
		fs.b_alloc().unwrap();
		let mut iroot = <<FSName as InodeSupport>::Inode as InodeLike>::new(
	        1,
	        &FType::TDir,
	        0,
	        // initially the inode contains no data
	        (1.5 * (BLOCK_SIZE as f32)) as u64,
	        &[4,5],
	    ).unwrap();
	    println!("{:?}", iroot);
	    fs.i_put(&iroot).unwrap();
		let i1 = <<FSName as InodeSupport>::Inode as InodeLike>::new(
	        2,
	        &FType::TFile,
	        0,
	        // initially the inode contains no data
	        0,
	        &[0],
	    ).unwrap();
		fs.i_put(&i1).unwrap();
		// put a directory entry across block boundaries
		println!("{:?}", *DIRENTRY_SIZE);
		let mut buffer = Buffer::new_zero(*DIRENTRY_SIZE);
		let direntry = FSName::new_de(2, "test").unwrap();
		buffer.serialize_into(&direntry, 0).unwrap();
		let offset = BLOCK_SIZE - (BLOCK_SIZE % *DIRENTRY_SIZE);
		fs.i_write(&mut iroot, &buffer, offset, *DIRENTRY_SIZE).unwrap();
		assert_eq!((i1, offset), fs.dirlookup(&iroot, "test").unwrap());
	}
}

// WARNING: DO NOT TOUCH THE BELOW CODE -- IT IS REQUIRED FOR TESTING -- YOU WILL LOSE POINTS IF I MANUALLY HAVE TO FIX YOUR TESTS
#[cfg(all(test, any(feature = "c", feature = "all")))]
#[path = "../../api/fs-tests/c_test.rs"]
mod tests;
