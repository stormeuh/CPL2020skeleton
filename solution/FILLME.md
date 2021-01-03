# FS

## Extra

Write down what extra tests you have written, if any. If you did something extra impressive let us know here as well!

I tried to keep the complexity of reading and writing things from and to disk as separated as possible from the actual operations being done on the in memory items by making heavy use of structs implementing the `Iterator` trait. A really good example of this is the i_write method from assignment e. The method works in three steps: first perform all the necessary checks, second create an iterator for doing the task, and third, execute the task. The iterator abstracts allocating new blocks to write to, and we know it won't run out because of the checks executed in step 1.
Along these lines, the following structs implement iterators (per assignment)
- A: `BitmapBlockIterator` iterates over the bitmap blocks of the file system. BitmapBlock is a wrapper around `Block`s which implements methods for allocation and deallocation. This uses hardware bit operations provided by `bitintr`.
- B: `InodeIterator` iterates over inodes in a file system. When the desired inode is found, it along with the `InodeBlock` containing it can be recovered. `InodeBlock` then contains a method to write it back to disk. This is another great example of abstracting the details of writing to disk away.
- C: At this point the pattern is obvious, `DirEntryIterator` iterates over DirEntries. However, as DirEntries can be written across block boundaries, this iterator keeps a `VecDeque` used as a FIFO queue to minimize the amount of times inode blocks are read. 
- D: uses the `DirEntryIterator` from C, to count the number of entries in an inode.
- E: implements an `InodeBlockIterator` and `InodeBlockMutIter`, the latter being an iterator which writes modification to its blocks back to disk.

Interesting extra tests are, per assignment:
- A: allocation is the only interesting test, the others are sanity checks for parts of the implementation.
- B: as suggested in `b_test.rs`, `inodes_over_multiple_blocks()` writes inodes over multiple blocks and reads them back manually .
- C: `test_dirlookup()` tests reading back a directory entry which spans over two inode blocks. `my_tests` implements some sanity checks for the directory name matching regex.
- D: For this one I felt the provided tests were sufficient. `my_tests` implements some sanity checks for the path name matching regex.
- E: This one I had the most difficulty with getting right, so `test_with_utils` basically tests all the possible edge cases for `i_write()` and `i_read()`.

## Feedback on the Project 

After working on this project for such a long time, you're allowed to vent in this
section. Tell us what you learned or what you would've liked to learn instead,
etc. This does not count toward your grades at all (neither positive nor negative).

- Some of the links to the rust book are outdated
- Having to use u64 instead of usize is really irritating
- in file c_test.rs:81 the size of the directory inode under test is set to a non whole multiple of the size of a directory entry. This caused a major headache as I wrote my functions under the assumption the size of an Inode of FType::TDir will always have a whole multiple size of DIRENTRY_SIZE. This should be the case if all directory entries are written by using dirlink.
- The InodeLike trait should have required setters in addition to getters for its fields in order to be fully transparent to the filesystems using them. Now you would call inode.get_inum() for example, only to then set it with inode.disk_node.inum = x. This breaks as soon as we set the inode type to something else than Inode. All other parts of my implementation would have needed minimal modifications if this were the case to implement assignment F, which is a bit frustrating.
