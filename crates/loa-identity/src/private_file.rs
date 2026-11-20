//! Create key files with a protected DACL; never write a key to an inherited ACL.

use std::fs::File;
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, GENERIC_WRITE, INVALID_HANDLE_VALUE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE, SET_ACCESS, SetEntriesInAclW, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACL, GetTokenInformation, InitializeSecurityDescriptor, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SE_DACL_PROTECTED,
    SetSecurityDescriptorControl, SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateFileW, FILE_ALL_ACCESS, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, GetVolumeInformationByHandleW,
};
use windows_sys::Win32::System::SystemServices::{FILE_PERSISTENT_ACLS, SECURITY_DESCRIPTOR_REVISION};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

struct Acl(*mut ACL);

impl Drop for Acl {
    fn drop(&mut self) {
        // SAFETY: SetEntriesInAclW returned this LocalAlloc allocation.
        unsafe { LocalFree(self.0.cast()) };
    }
}

pub(super) fn create(path: &Path) -> io::Result<File> {
    let mut token = null_mut();
    // SAFETY: the current-process pseudo-handle is valid; `token` receives an owned handle.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut needed = 0;
    // SAFETY: a zero-length buffer asks for the size.
    unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    // A TOKEN_USER and its SID must be aligned and remain alive until the ACL is built.
    let mut buffer = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
    if unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, buffer.as_mut_ptr().cast(), needed, &mut needed) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let access = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS,
        grfAccessMode: SET_ACCESS,
        grfInheritance: 0,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: sid.cast(),
        },
    };
    let mut acl = null_mut();
    let result = unsafe { SetEntriesInAclW(1, &access, null(), &mut acl) };
    if result != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(result as i32));
    }
    let acl = Acl(acl);
    let mut descriptor = SECURITY_DESCRIPTOR::default();
    let desc = (&raw mut descriptor).cast();
    // SAFETY: descriptor and ACL live through CreateFileW. The protected DACL prevents inheritance.
    if unsafe {
        InitializeSecurityDescriptor(desc, SECURITY_DESCRIPTOR_REVISION) == 0
            || SetSecurityDescriptorDacl(desc, 1, acl.0, 0) == 0
            || SetSecurityDescriptorControl(desc, SE_DACL_PROTECTED, SE_DACL_PROTECTED) == 0
    } {
        return Err(io::Error::last_os_error());
    }
    let attrs = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: desc, bInheritHandle: 0 };
    let wide: Vec<_> = path.as_os_str().encode_wide().chain([0]).collect();
    if wide[..wide.len() - 1].contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "key path contains NUL"));
    }
    // SAFETY: `wide` is NUL terminated and `attrs` points to a live descriptor.
    // CREATE_NEW won't open a pre-existing path; share-delete allows atomic publish.
    let handle = unsafe { CreateFileW(wide.as_ptr(), GENERIC_WRITE, FILE_SHARE_DELETE, &attrs, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, null_mut()) };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let file = File::from(unsafe { OwnedHandle::from_raw_handle(handle) });
    // FAT and exFAT ignore the DACL; never write a key that anyone can read.
    let mut flags = 0;
    // SAFETY: `file` is an open handle; unused outputs are null with zero sizes.
    let queried = unsafe {
        GetVolumeInformationByHandleW(file.as_raw_handle(), null_mut(), 0, null_mut(), null_mut(), &mut flags, null_mut(), 0)
    };
    if queried == 0 || flags & FILE_PERSISTENT_ACLS == 0 {
        let error = if queried == 0 {
            io::Error::last_os_error()
        } else {
            io::Error::new(io::ErrorKind::Unsupported, "the key folder's volume can't restrict file access")
        };
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(file)
}
