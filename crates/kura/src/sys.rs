//! libc's types and functions, with the structs whose layout the libc crate gets wrong for
//! emscripten's wasm64 (MEMORY64, the 64-bit build) replaced by the C one. wasm32: libc's as is.
pub use libc::*;

// (items, not a module: they shadow the glob import)
macro_rules! wasm64 {
    ($($i:item)*) => { $(#[cfg(target_arch = "wasm64")] $i)* };
}

wasm64! {

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct stat {
        pub st_dev: dev_t,
        pub st_mode: mode_t,
        pub st_nlink: u64,
        pub st_uid: uid_t,
        pub st_gid: gid_t,
        pub st_rdev: dev_t,
        pub st_size: off_t,
        pub st_blksize: c_int,
        pub st_blocks: c_int,
        pub st_atime: time_t,
        pub st_atime_nsec: c_long,
        pub st_mtime: time_t,
        pub st_mtime_nsec: c_long,
        pub st_ctime: time_t,
        pub st_ctime_nsec: c_long,
        pub st_ino: ino_t,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct statvfs {
        pub f_bsize: c_ulong,
        pub f_frsize: c_ulong,
        pub f_blocks: u64,
        pub f_bfree: u64,
        pub f_bavail: u64,
        pub f_files: u64,
        pub f_ffree: u64,
        pub f_favail: u64,
        pub f_fsid: c_ulong,
        pub f_flag: c_ulong,
        pub f_namemax: c_ulong,
        pub f_type: c_uint,
        __f_spare: [c_int; 5],
    }

    #[repr(C)]
    pub struct msghdr {
        pub msg_name: *mut c_void,
        pub msg_namelen: socklen_t,
        pub msg_iov: *mut iovec,
        pub msg_iovlen: c_int,
        __pad1: c_int,
        pub msg_control: *mut c_void,
        pub msg_controllen: socklen_t,
        __pad2: c_int,
        pub msg_flags: c_int,
    }

    #[repr(C)]
    pub struct cmsghdr {
        pub cmsg_len: socklen_t,
        __pad1: c_int,
        pub cmsg_level: c_int,
        pub cmsg_type: c_int,
    }

    const fn align(n: usize) -> usize {
        (n + 7) & !7
    }
    pub const fn CMSG_LEN(len: c_uint) -> c_uint {
        (align(core::mem::size_of::<cmsghdr>()) + len as usize) as c_uint
    }
    pub const fn CMSG_SPACE(len: c_uint) -> c_uint {
        (align(len as usize) + align(core::mem::size_of::<cmsghdr>())) as c_uint
    }
    pub unsafe fn CMSG_DATA(c: *const cmsghdr) -> *mut c_uchar {
        c.add(1) as *mut c_uchar
    }
    pub unsafe fn CMSG_FIRSTHDR(m: *const msghdr) -> *mut cmsghdr {
        if (*m).msg_controllen as usize >= core::mem::size_of::<cmsghdr>() { (*m).msg_control as *mut cmsghdr } else { core::ptr::null_mut() }
    }
    pub unsafe fn CMSG_NXTHDR(m: *const msghdr, c: *const cmsghdr) -> *mut cmsghdr {
        let len = (*c).cmsg_len as usize;
        let end = (*m).msg_control as usize + (*m).msg_controllen as usize;
        if len < core::mem::size_of::<cmsghdr>() || align(len) + core::mem::size_of::<cmsghdr>() >= end - c as usize {
            return core::ptr::null_mut();
        }
        (c as *const u8).add(align(len)) as *mut cmsghdr
    }

    extern "C" {
        pub fn fstat(fd: c_int, st: *mut stat) -> c_int;
    }
}
