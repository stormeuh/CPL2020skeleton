//! File system with path support
//!
//! Create a filesystem that has a notion of blocks, inodes, directory inodes and paths, by implementing the [`FileSysSupport`], the [`BlockSupport`], the [`InodeSupport`], the [`DirectorySupport`] and the [`PathSupport`] traits together (again, all earlier traits are supertraits of the later ones).
//!
//! [`FileSysSupport`]: ../../cplfs_api/fs/trait.FileSysSupport.html
//! [`BlockSupport`]: ../../cplfs_api/fs/trait.BlockSupport.html
//! [`InodeSupport`]: ../../cplfs_api/fs/trait.InodeSupport.html
//! [`DirectorySupport`]: ../../cplfs_api/fs/trait.DirectorySupport.html
//! [`PathSupport`]: ../../cplfs_api/fs/trait.PathSupport.html
//! Make sure this file does not contain any unaddressed `TODO`s anymore when you hand it in.
//!
//! # Status
//!
//! **TODO**: Replace the question mark below with YES, NO, or PARTIAL to
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

// BFS for Backing FileSystem
use regex::Regex;
use crate::c_dirs_support::FSName as BFSName;
use crate::c_dirs_support::{RE_DIRNAME_INNER, DirectoryError, DirEntryIterator};
use thiserror::Error;
use std::path::Path;
use lazy_static::lazy_static;

use cplfs_api::fs::{FileSysSupport, BlockSupport, InodeSupport, InodeRWSupport, DirectorySupport, PathSupport};
use cplfs_api::types::{DirEntry, DIRENTRY_SIZE};
use cplfs_api::types::{Inode, InodeLike, FType};
use cplfs_api::types::{SuperBlock, Block, Buffer};
use cplfs_api::controller::{Device};


/// You are free to choose the name for your file system. As we will use
/// automated tests when grading your assignment, indicate here the name of
/// your file system data type so we can just use `FSName` instead of
/// having to manually figure out the name.
pub type FSName = EsFS4;

/// A filesystem that supports paths
pub struct EsFS4 {
	/// Backing directory supporting filesystem
	dfs: BFSName,
	/// Current working directory, formatted as direntry names implicitly separated by /
	cwd: Vec<String>,
}

lazy_static!{
	// shoutout to [Debuggex](https://www.debuggex.com/) for their visualisations
	static ref RE_PATH: Regex = Regex::new(format!("^(\\.{{1,2}})$|^(\\.{{0,2}})(((/){})+|(/))$", *RE_DIRNAME_INNER).as_str()).unwrap();
}

/// Errors which may be thrown by [`EsFS3`](struct.EsFS3.html).
#[derive(Error, Debug)]
pub enum PathError {
	/// Error during initialisation of filesystem
	#[error("Error during initialisation of filesystem: {0}")]
	Init(&'static str),
	/// Path is invalid
	#[error("Path is invalid: {0}")]
	Path(&'static str),
	/// Problem with lookup
	#[error("Problem during path resolution: {0}")]
	Lookup(String),
	/// Problem performing unlink
	#[error("Problem unlinking entry: {0}")]
	Unlink(&'static str),
	/// Error caused in the lower filesystem layers
	#[error("Error in the lower layers")]
	Backend(#[from] <BFSName as FileSysSupport>::Error),
}

impl PathSupport for EsFS4 {
	fn valid_path(path: &str) -> bool {
		RE_PATH.is_match(path)
	}

	fn get_cwd(&self) -> String {
		let mut cwd_string = String::new();
		if self.cwd.len() == 0 {
			cwd_string.push_str("/");
		} else {
			for dn in self.cwd.iter() {
				cwd_string.push_str("/");
				cwd_string.push_str(&dn);
			}
		}
		cwd_string
	}	

	fn set_cwd(&mut self, path: &str) -> Option<()> {
		let dirnames = EsFS4::into_dirnames(path)?;
		if dirnames.len() == 0 {
			self.cwd = Vec::new();
		} else {
			self.cwd = self.flatten_path(dirnames);
		}
		Some(())
	}

	fn resolve_path(&self, path: &str) -> Result<Self::Inode, Self::Error> {
		let path_dirnames = match Self::into_dirnames(path) {
			Some(paths) => paths,
			None => return Err(PathError::Path("invalid format.")),
		};
		// start at root
		let mut curr_inode = self.i_get(1)?;
		println!("root: {:?}", curr_inode);
		if path_dirnames[0] == "." || path_dirnames[0] == ".." {
			for dirname in &self.cwd {
				curr_inode = self.lookup_dirname_inode(&curr_inode, &dirname)?;
				println!("{}, {:?}", dirname, curr_inode);
			}
		}
		for dirname in path_dirnames {
			curr_inode = self.lookup_dirname_inode(&curr_inode, &dirname)?;
			println!("{}, {:?}", dirname, curr_inode);
		}
		Ok(curr_inode)
	}

	fn mkdir(&mut self, path: &str) -> Result<Self::Inode, Self::Error> {
		let (prefix, new_dirname) = match Self::split_parent_entry(path) {
			Some((_, "..")) => { return Err(PathError::Path(".. is an invalid directory name."))},
			Some((_, "."))  => { return Err(PathError::Path(". is an invalid directory name."))},
			Some(paths) => paths,
			None => return Err(PathError::Path("invalid format.")),
		};
		let mut parent_inode = self.resolve_path(prefix)?;
		// allocate new inode, make link in parent
		let new_dir_inum = self.i_alloc(FType::TDir)?;
		self.dirlink(&mut parent_inode, new_dirname, new_dir_inum)?;
		// make link to parent and self in new inode
		let mut new_dir_inode = self.i_get(new_dir_inum)?;
		self.dirlink(&mut new_dir_inode, ".", new_dir_inum)?;
		self.dirlink(&mut new_dir_inode, "..", parent_inode.get_inum())?;
		Ok(new_dir_inode)
	}

	fn unlink(&mut self, path: &str) -> Result<(), Self::Error> {
		println!("{:?}", path);
		let (prefix, to_remove) = match Self::split_parent_entry(path) {
			Some((_, "..")) => { return Err(PathError::Unlink("cannot remove entry .. as it is fixed."))},
			Some((_, "."))  => { return Err(PathError::Unlink("cannot remove entry . as it is fixed."))},
			Some(paths) => paths,
			None => return Err(PathError::Path("invalid format.")),
		};
		// retrieve parent and entry inodes
		let mut parent_inode = self.resolve_path(prefix)?;
		let (mut to_remove_inode, offset) = self.dirlookup(&parent_inode, to_remove)?;
		// count number of directory entries in the inode to remove, if non empty, throw error
		match self.count_entries(&to_remove_inode) {
			Some(num_entries) => {
				if num_entries > 2 {
					return Err(PathError::Unlink("cannot remove non-empty directory."));
				}
			},
			None => {},
		}
		// create all zero buffer of direntry size, write at offset in directory
		let buffer = Buffer::new_zero(*DIRENTRY_SIZE);
		self.i_write(&mut parent_inode, &buffer, offset, *DIRENTRY_SIZE)?;
		// check if self reference, otherwise decrement nlink of entry
		if parent_inode.get_inum() != to_remove_inode.get_inum() {
			// overflowing_sub returns self when overflow occurs
			let (new_nlink, _) = to_remove_inode.get_nlink().overflowing_sub(1u64);
			to_remove_inode.disk_node.nlink = new_nlink as u16;
			// write back to disk
			self.i_put(&to_remove_inode)?;
			// if nlink is zero, free the inode as well, also decrementing the nlink of the parent
			if new_nlink == 0 {
				match self.dirlookup(&to_remove_inode, "..") {
					Ok((mut real_parent, _)) => {
						real_parent.disk_node.nlink -= 1;
						self.i_put(&real_parent)?;
					},
					Err(PathError::Backend(DirectoryError::ItemNotFound)) => {},
					Err(e) => return Err(e),
				}
				self.i_free(to_remove_inode.get_inum())?;
			}
		}
		Ok(())
	}
}

impl EsFS4 {
	fn flatten_path(&self, dirnames: Vec<&str>) -> Vec<String> {
		let mut path = match dirnames[0] {
			"." => self.cwd.clone(),
			".." => self.cwd.clone(),
			// path is absolute, reset to root
			_ => Vec::new(),
		};
		// iterate over the captured dirnames, modifying cwd as needed
		for dirname in dirnames {
			match dirname {
				// remain in current directory, nothing has to be done
				"." => {},
				// move up one directory
				".." => {
					// pop a dirname from path, if it is empty this does nothing, which
					// is fine as in that case we are at root
					path.pop();
				},
				// append a directory
				dirname => {
					path.push(dirname.to_string())
				},
			}
		}
		path
	}

	fn lookup_dirname_inode(&self, inode: &Inode, dirname: &str) -> Result<Inode, PathError> {
		match self.dirlookup(inode, dirname) {
			Err(PathError::Backend(DirectoryError::IllegalInode(_))) => {
				let mut err_msg = String::new();
				err_msg.push_str(dirname);
				err_msg.push_str(" is not a directory.");
				return Err(PathError::Lookup(err_msg))
			},
			Err(PathError::Backend(DirectoryError::ItemNotFound)) => {
				let mut err_msg = String::new();
				err_msg.push_str(dirname);
				err_msg.push_str(" could not be found.");
				return Err(PathError::Lookup(err_msg))
			},
			Err(e) => return Err(e),
			Ok((found_inode, _)) => {
				return Ok(found_inode)
			},
		}
	}

	/// Parses string path into vector of dirnames
	/// Returns none if the path is improperly formatted
	fn into_dirnames(path: &str) -> Option<Vec<&str>> {
		// using a regex here may result in worse performance than just regular split, 
		// but it gives more flexibility in the future
		if !Self::valid_path(path) {return None}
		let mut dirnames = Vec::new();
		for capture in RE_DIRNAME_INNER.captures_iter(path) {
			dirnames.push(capture.get(0).unwrap().as_str())
		}
		Some(dirnames)
	}

	/// Splits off the entry name from the parent path
	fn split_parent_entry(path: &str) -> Option<(&str, &str)> {
		let path_dirnames = match Self::into_dirnames(path) {
			Some(paths) => paths,
			None => return None,
		};
		if path_dirnames.len() > 1 {
			// get dirname from parsed path
			let new_dirname = *path_dirnames.last().unwrap();
			let prefix_range = 0..(path.len() - new_dirname.len() - 1);
			return Some((&path[prefix_range], &new_dirname))
		} else {
			return Some((&"", &path_dirnames[0]))
		}
	}

	/// counts the number of non empty entries in the directory represented by inode
	/// Returns none if the given inode is not a directory
	fn count_entries(&self, inode: &Inode) -> Option<u64> {
		let mut num_entries = 0;
		// create iterator over direntries, if error the given inode is not a directory => map to none
		let entry_iter = DirEntryIterator::new(&self.dfs, inode).ok()?;
		for de in entry_iter {
			if de.inum != 0 {num_entries += 1};
			println!("{:?}", de);
		}
		Some(num_entries)
	}
}

impl FileSysSupport for EsFS4 {
	type Error = PathError;

	fn sb_valid(sb: &SuperBlock) -> bool {
		<BFSName as FileSysSupport>::sb_valid(sb)
	}

	fn mkfs<P: AsRef<Path>>(path: P, sb: &SuperBlock) -> Result<Self, Self::Error> {
		// create new inode fs as backend
		let dir_fs = BFSName::mkfs(path, sb)?;
		// wrap into directory supporting fs
		let mut new_fs = EsFS4 {
			dfs: dir_fs,
			cwd: Vec::new(),
		};
		let mut iroot = new_fs.i_get(1)?;
		new_fs.dirlink(&mut iroot, ".", 1)?;
		new_fs.dirlink(&mut iroot, "..", 1)?;
		Ok(new_fs)
	}

	fn mountfs(dev: Device) -> Result<Self, Self::Error> {
		// mount device in inode fs, and wrap it
		let inode_fs = BFSName::mountfs(dev)?;
		Ok(Self {dfs: inode_fs, cwd: Vec::new(),})
	}

	fn unmountfs(self) -> Device {
		self.dfs.unmountfs()
	}
}

// mapping of directory operations onto backing filesystem
impl DirectorySupport for EsFS4 {
	fn new_de(inum: u64, name: &str) -> Option<DirEntry> {
		<BFSName as DirectorySupport>::new_de(inum, name)
	}

	fn get_name_str(de: &DirEntry) -> String {
		<BFSName as DirectorySupport>::get_name_str(de)
	}

	fn set_name_str(de: &mut DirEntry, name: &str) -> Option<()> {
		<BFSName as DirectorySupport>::set_name_str(de, name)	
	}

	fn dirlookup(
		&self, 
		inode: &Self::Inode, 
		name: &str
	) -> Result<(Self::Inode, u64), Self::Error> {
		Ok(self.dfs.dirlookup(inode, name)?)
    }

    fn dirlink(
        &mut self,
        inode: &mut Self::Inode,
        name: &str,
        inum: u64,
    ) -> Result<u64, Self::Error> {
    	Ok(self.dfs.dirlink(inode, name, inum)?)
    }
}

// mapping inode RW operations onto backing filesystem
impl InodeRWSupport for EsFS4 {
	fn i_read(&self, 
		inode: &Self::Inode, 
		buf: &mut Buffer, 
		off: u64, 
		n: u64
	) -> Result<u64, Self::Error> {
		Ok(self.dfs.i_read(inode, buf, off, n)?)
	}

	fn i_write(&mut self, 
		inode: &mut Self::Inode, 
		buf: &Buffer, 
		off: u64, 
		n: u64
	) -> Result<(), Self::Error> {
		Ok(self.dfs.i_write(inode, buf, off, n)?)
	}
}

// mapping of inode operations onto backing filesystem
impl InodeSupport for EsFS4 {
	type Inode = <BFSName as InodeSupport>::Inode;

	fn i_get(&self, i: u64) -> Result<Self::Inode, Self::Error> {
		Ok(self.dfs.i_get(i)?)
	}

	fn i_put(&mut self, ino: &Self::Inode) -> Result<(), Self::Error> {
		Ok(self.dfs.i_put(ino)?)
	}

	fn i_free(&mut self, i: u64) -> Result<(), Self::Error> {
		Ok(self.dfs.i_free(i)?) 
	}

	fn i_alloc(&mut self, ft: FType) -> Result<u64, Self::Error> {
		Ok(self.dfs.i_alloc(ft)?)
	}

	fn i_trunc(&mut self, inode: &mut Self::Inode) -> Result<(), Self::Error> {
		Ok(self.dfs.i_trunc(inode)?)
	}
}

// mapping of block operations onto backing filesystem
impl BlockSupport for EsFS4 {
	fn b_get(&self, i: u64) -> std::result::Result<Block, <Self as FileSysSupport>::Error> { 
		Ok(self.dfs.b_get(i)?)
	}
	fn b_put(&mut self, b: &Block) -> std::result::Result<(), <Self as FileSysSupport>::Error> { 
		Ok(self.dfs.b_put(b)?)
	}
	fn b_free(&mut self, i: u64) -> std::result::Result<(), <Self as FileSysSupport>::Error> { 
		Ok(self.dfs.b_free(i)?)
	}
	fn b_zero(&mut self, i: u64) -> std::result::Result<(), <Self as FileSysSupport>::Error> { 
		Ok(self.dfs.b_zero(i)?)
	}
	fn b_alloc(&mut self) -> std::result::Result<u64, <Self as FileSysSupport>::Error> { 
		Ok(self.dfs.b_alloc()?)
	}
	fn sup_get(&self) -> std::result::Result<SuperBlock, <Self as FileSysSupport>::Error> { 
		Ok(self.dfs.sup_get()?)
	}
	fn sup_put(&mut self, sb: &SuperBlock) -> std::result::Result<(), <Self as FileSysSupport>::Error> { 
		Ok(self.dfs.sup_put(sb)?)
	}
}

#[cfg(test)]
mod my_tests {
	use super::{RE_PATH};
	use super::FSName;
	use cplfs_api::fs::DirectorySupport;

	#[test]
	fn test_path_regex(){
		assert!(!RE_PATH.is_match(""));
		// assert!(RE_PATH.is_match("."));
		// assert!(RE_PATH.is_match(".."));
		assert!(RE_PATH.is_match("/"));
		assert!(RE_PATH.is_match("./"));
		assert!(RE_PATH.is_match("../"));
		assert!(RE_PATH.is_match("./in35"));
		assert!(!RE_PATH.is_match(".../"));
		assert!(!RE_PATH.is_match("/may/not/end/in/"));
		assert!(RE_PATH.is_match("/verylongnameee/../test/multiple/./level/../deep"));
		assert!(RE_PATH.is_match("./verylongnameee/../test/multiple/./level/../deep"));
		assert!(RE_PATH.is_match("../verylongnameee/../test/multiple/./level/../deep"));
		assert!(!RE_PATH.is_match(".../verylongnameee/../test/multiple/./level/../deep"));
		assert!(!RE_PATH.is_match("../justtoolongname/../test/multiple/./level/../deep"));
		assert!(!RE_PATH.is_match("../exceedinglylongname/../test/multiple/./level/../deep"));
	}

	#[test]
	fn test_new_de() {
		// verify name error is actually passed on through new_de
		assert!(!FSName::new_de(1, "root").is_none());
		assert!(FSName::new_de(1, "exceedinglylongname").is_none());
	}

	#[test]
	fn test_into_dirnames() {
		assert_eq!(FSName::into_dirnames("./in35").unwrap(), vec![".", "in35"]);
	}
}

// WARNING: DO NOT TOUCH THE BELOW CODE -- IT IS REQUIRED FOR TESTING -- YOU WILL LOSE POINTS IF I MANUALLY HAVE TO FIX YOUR TESTS
#[cfg(all(test, any(feature = "d", feature = "all")))]
#[path = "../../api/fs-tests/d_test.rs"]
mod tests;
