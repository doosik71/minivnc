//! System V shared memory segment for MIT-SHM `ShmGetImage`. This is the
//! crate's only `unsafe` code, confined to the libc calls.
#![allow(unsafe_code)]

use std::io;

/// A private shared memory segment attached to this process.
pub(super) struct Segment {
    id: i32,
    addr: *mut u8,
    size: usize,
}

// SAFETY: the mapping is owned exclusively by this value.
unsafe impl Send for Segment {}

impl Segment {
    pub(super) fn new(size: usize) -> io::Result<Self> {
        // SAFETY: plain syscall wrappers; results are checked.
        let id = unsafe { libc::shmget(libc::IPC_PRIVATE, size, libc::IPC_CREAT | 0o600) };
        if id < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `id` is a segment we just created.
        let addr = unsafe { libc::shmat(id, std::ptr::null(), 0) };
        if addr as isize == -1 {
            let err = io::Error::last_os_error();
            // SAFETY: as above.
            unsafe { libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut()) };
            return Err(err);
        }
        Ok(Self {
            id,
            addr: addr.cast(),
            size,
        })
    }

    pub(super) fn id(&self) -> u32 {
        self.id as u32
    }

    /// Marks the segment for removal once every process detaches. Call
    /// after the X server attached it.
    pub(super) fn mark_removed(&self) {
        // SAFETY: valid segment id.
        unsafe { libc::shmctl(self.id, libc::IPC_RMID, std::ptr::null_mut()) };
    }

    pub(super) fn size(&self) -> usize {
        self.size
    }

    /// The first `len` bytes. Only read after the X server's reply to a
    /// request that wrote them, so nothing writes concurrently.
    pub(super) fn bytes(&self, len: usize) -> &[u8] {
        assert!(len <= self.size, "read past shared memory segment");
        // SAFETY: the mapping is `size` bytes long and lives as long as self.
        unsafe { std::slice::from_raw_parts(self.addr, len) }
    }
}

impl Drop for Segment {
    fn drop(&mut self) {
        // SAFETY: detaching our own mapping; IPC_RMID again is harmless.
        unsafe {
            libc::shmdt(self.addr.cast());
            libc::shmctl(self.id, libc::IPC_RMID, std::ptr::null_mut());
        }
    }
}
