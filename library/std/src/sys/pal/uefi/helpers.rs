//! Contains most of the shared UEFI specific stuff. Some of this might be moved to `std::os::uefi`
//! if needed but no point in adding extra public API when there is not Std support for UEFI in the
//! first place
//!
//! Some Nomenclature
//! * Protocol:
//! - Protocols serve to enable communication between separately built modules, including drivers.
//! - Every protocol has a GUID associated with it. The GUID serves as the name for the protocol.
//! - Protocols are produced and consumed.
//! - More information about protocols can be found [here](https://edk2-docs.gitbook.io/edk-ii-uefi-driver-writer-s-guide/3_foundation/36_protocols_and_handles)

use r_efi::efi::{self, Guid};
use r_efi::protocols::{device_path, device_path_to_text, service_binding, shell};

use crate::ffi::{OsStr, OsString};
use crate::io::{self, const_error};
use crate::marker::PhantomData;
use crate::mem::MaybeUninit;
use crate::os::uefi::env::boot_services;
use crate::os::uefi::ffi::{OsStrExt, OsStringExt};
use crate::os::uefi::{self};
use crate::path::Path;
use crate::ptr::NonNull;
use crate::slice;
use crate::sync::atomic::{Atomic, AtomicPtr, Ordering};
use crate::sys_common::wstr::WStrUnits;

type BootInstallMultipleProtocolInterfaces =
    unsafe extern "efiapi" fn(_: *mut r_efi::efi::Handle, _: ...) -> r_efi::efi::Status;

type BootUninstallMultipleProtocolInterfaces =
    unsafe extern "efiapi" fn(_: r_efi::efi::Handle, _: ...) -> r_efi::efi::Status;

const BOOT_SERVICES_UNAVAILABLE: io::Error =
    const_error!(io::ErrorKind::Other, "Boot Services are no longer available");

/// Locates Handles with a particular Protocol GUID.
///
/// Implemented using `EFI_BOOT_SERVICES.LocateHandles()`.
///
/// Returns an array of [Handles](r_efi::efi::Handle) that support a specified protocol.
pub(crate) fn locate_handles(mut guid: Guid) -> io::Result<Vec<NonNull<crate::ffi::c_void>>> {
    fn inner(
        guid: &mut Guid,
        boot_services: NonNull<r_efi::efi::BootServices>,
        buf_size: &mut usize,
        buf: *mut r_efi::efi::Handle,
    ) -> io::Result<()> {
        let r = unsafe {
            ((*boot_services.as_ptr()).locate_handle)(
                r_efi::efi::BY_PROTOCOL,
                guid,
                crate::ptr::null_mut(),
                buf_size,
                buf,
            )
        };

        if r.is_error() { Err(crate::io::Error::from_raw_os_error(r.as_usize())) } else { Ok(()) }
    }

    let boot_services = boot_services().ok_or(BOOT_SERVICES_UNAVAILABLE)?.cast();
    let mut buf_len = 0usize;

    // This should always fail since the size of buffer is 0. This call should update the buf_len
    // variable with the required buffer length
    match inner(&mut guid, boot_services, &mut buf_len, crate::ptr::null_mut()) {
        Ok(()) => unreachable!(),
        Err(e) => match e.kind() {
            io::ErrorKind::FileTooLarge => {}
            _ => return Err(e),
        },
    }

    // The returned buf_len is in bytes
    assert_eq!(buf_len % size_of::<r_efi::efi::Handle>(), 0);
    let num_of_handles = buf_len / size_of::<r_efi::efi::Handle>();
    let mut buf: Vec<r_efi::efi::Handle> = Vec::with_capacity(num_of_handles);
    match inner(&mut guid, boot_services, &mut buf_len, buf.as_mut_ptr()) {
        Ok(()) => {
            // This is safe because the call will succeed only if buf_len >= required length.
            // Also, on success, the `buf_len` is updated with the size of bufferv (in bytes) written
            unsafe { buf.set_len(num_of_handles) };
            Ok(buf.into_iter().filter_map(|x| NonNull::new(x)).collect())
        }
        Err(e) => Err(e),
    }
}

/// Open Protocol on a handle.
/// Internally just a call to `EFI_BOOT_SERVICES.OpenProtocol()`.
///
/// Queries a handle to determine if it supports a specified protocol. If the protocol is
/// supported by the handle, it opens the protocol on behalf of the calling agent.
pub(crate) fn open_protocol<T>(
    handle: NonNull<crate::ffi::c_void>,
    mut protocol_guid: Guid,
) -> io::Result<NonNull<T>> {
    let boot_services: NonNull<efi::BootServices> =
        boot_services().ok_or(BOOT_SERVICES_UNAVAILABLE)?.cast();
    let system_handle = uefi::env::image_handle();
    let mut protocol: MaybeUninit<*mut T> = MaybeUninit::uninit();

    let r = unsafe {
        ((*boot_services.as_ptr()).open_protocol)(
            handle.as_ptr(),
            &mut protocol_guid,
            protocol.as_mut_ptr().cast(),
            system_handle.as_ptr(),
            crate::ptr::null_mut(),
            r_efi::system::OPEN_PROTOCOL_GET_PROTOCOL,
        )
    };

    if r.is_error() {
        Err(crate::io::Error::from_raw_os_error(r.as_usize()))
    } else {
        NonNull::new(unsafe { protocol.assume_init() })
            .ok_or(const_error!(io::ErrorKind::Other, "null protocol"))
    }
}

/// Gets the Protocol for current system handle.
///
/// Note: Some protocols need to be manually freed. It is the caller's responsibility to do so.
pub(crate) fn image_handle_protocol<T>(protocol_guid: Guid) -> io::Result<NonNull<T>> {
    let system_handle = uefi::env::try_image_handle()
        .ok_or(io::const_error!(io::ErrorKind::NotFound, "protocol not found in Image handle"))?;
    open_protocol(system_handle, protocol_guid)
}

pub(crate) fn device_path_to_text(path: NonNull<device_path::Protocol>) -> io::Result<OsString> {
    fn path_to_text(
        protocol: NonNull<device_path_to_text::Protocol>,
        path: NonNull<device_path::Protocol>,
    ) -> io::Result<OsString> {
        let path_ptr: *mut r_efi::efi::Char16 = unsafe {
            ((*protocol.as_ptr()).convert_device_path_to_text)(
                path.as_ptr(),
                // DisplayOnly
                r_efi::efi::Boolean::FALSE,
                // AllowShortcuts
                r_efi::efi::Boolean::FALSE,
            )
        };

        let path = os_string_from_raw(path_ptr)
            .ok_or(io::const_error!(io::ErrorKind::InvalidData, "invalid path"))?;

        if let Some(boot_services) = crate::os::uefi::env::boot_services() {
            let boot_services: NonNull<r_efi::efi::BootServices> = boot_services.cast();
            unsafe {
                ((*boot_services.as_ptr()).free_pool)(path_ptr.cast());
            }
        }

        Ok(path)
    }

    static LAST_VALID_HANDLE: Atomic<*mut crate::ffi::c_void> =
        AtomicPtr::new(crate::ptr::null_mut());

    if let Some(handle) = NonNull::new(LAST_VALID_HANDLE.load(Ordering::Acquire)) {
        if let Ok(protocol) = open_protocol::<device_path_to_text::Protocol>(
            handle,
            device_path_to_text::PROTOCOL_GUID,
        ) {
            return path_to_text(protocol, path);
        }
    }

    let device_path_to_text_handles = locate_handles(device_path_to_text::PROTOCOL_GUID)?;
    for handle in device_path_to_text_handles {
        if let Ok(protocol) = open_protocol::<device_path_to_text::Protocol>(
            handle,
            device_path_to_text::PROTOCOL_GUID,
        ) {
            LAST_VALID_HANDLE.store(handle.as_ptr(), Ordering::Release);
            return path_to_text(protocol, path);
        }
    }

    Err(io::const_error!(io::ErrorKind::NotFound, "no device path to text protocol found"))
}

fn device_node_to_text(path: NonNull<device_path::Protocol>) -> io::Result<OsString> {
    fn node_to_text(
        protocol: NonNull<device_path_to_text::Protocol>,
        path: NonNull<device_path::Protocol>,
    ) -> io::Result<OsString> {
        let path_ptr: *mut r_efi::efi::Char16 = unsafe {
            ((*protocol.as_ptr()).convert_device_node_to_text)(
                path.as_ptr(),
                // DisplayOnly
                r_efi::efi::Boolean::FALSE,
                // AllowShortcuts
                r_efi::efi::Boolean::FALSE,
            )
        };

        let path = os_string_from_raw(path_ptr)
            .ok_or(io::const_error!(io::ErrorKind::InvalidData, "Invalid path"))?;

        if let Some(boot_services) = crate::os::uefi::env::boot_services() {
            let boot_services: NonNull<r_efi::efi::BootServices> = boot_services.cast();
            unsafe {
                ((*boot_services.as_ptr()).free_pool)(path_ptr.cast());
            }
        }

        Ok(path)
    }

    static LAST_VALID_HANDLE: AtomicPtr<crate::ffi::c_void> =
        AtomicPtr::new(crate::ptr::null_mut());

    if let Some(handle) = NonNull::new(LAST_VALID_HANDLE.load(Ordering::Acquire)) {
        if let Ok(protocol) = open_protocol::<device_path_to_text::Protocol>(
            handle,
            device_path_to_text::PROTOCOL_GUID,
        ) {
            return node_to_text(protocol, path);
        }
    }

    let device_path_to_text_handles = locate_handles(device_path_to_text::PROTOCOL_GUID)?;
    for handle in device_path_to_text_handles {
        if let Ok(protocol) = open_protocol::<device_path_to_text::Protocol>(
            handle,
            device_path_to_text::PROTOCOL_GUID,
        ) {
            LAST_VALID_HANDLE.store(handle.as_ptr(), Ordering::Release);
            return node_to_text(protocol, path);
        }
    }

    Err(io::const_error!(io::ErrorKind::NotFound, "No device path to text protocol found"))
}

/// Gets RuntimeServices.
pub(crate) fn runtime_services() -> Option<NonNull<r_efi::efi::RuntimeServices>> {
    let system_table: NonNull<r_efi::efi::SystemTable> =
        crate::os::uefi::env::try_system_table()?.cast();
    let runtime_services = unsafe { (*system_table.as_ptr()).runtime_services };
    NonNull::new(runtime_services)
}

pub(crate) struct OwnedDevicePath(NonNull<r_efi::protocols::device_path::Protocol>);

impl OwnedDevicePath {
    pub(crate) fn from_text(p: &OsStr) -> io::Result<Self> {
        fn inner(
            p: &OsStr,
            protocol: NonNull<r_efi::protocols::device_path_from_text::Protocol>,
        ) -> io::Result<OwnedDevicePath> {
            let path_vec = p.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
            if path_vec[..path_vec.len() - 1].contains(&0) {
                return Err(const_error!(
                    io::ErrorKind::InvalidInput,
                    "strings passed to UEFI cannot contain NULs",
                ));
            }

            let path =
                unsafe { ((*protocol.as_ptr()).convert_text_to_device_path)(path_vec.as_ptr()) };

            NonNull::new(path)
                .map(OwnedDevicePath)
                .ok_or_else(|| const_error!(io::ErrorKind::InvalidFilename, "invalid Device Path"))
        }

        static LAST_VALID_HANDLE: Atomic<*mut crate::ffi::c_void> =
            AtomicPtr::new(crate::ptr::null_mut());

        if let Some(handle) = NonNull::new(LAST_VALID_HANDLE.load(Ordering::Acquire)) {
            if let Ok(protocol) = open_protocol::<r_efi::protocols::device_path_from_text::Protocol>(
                handle,
                r_efi::protocols::device_path_from_text::PROTOCOL_GUID,
            ) {
                return inner(p, protocol);
            }
        }

        let handles = locate_handles(r_efi::protocols::device_path_from_text::PROTOCOL_GUID)?;
        for handle in handles {
            if let Ok(protocol) = open_protocol::<r_efi::protocols::device_path_from_text::Protocol>(
                handle,
                r_efi::protocols::device_path_from_text::PROTOCOL_GUID,
            ) {
                LAST_VALID_HANDLE.store(handle.as_ptr(), Ordering::Release);
                return inner(p, protocol);
            }
        }

        io::Result::Err(const_error!(
            io::ErrorKind::NotFound,
            "DevicePathFromText Protocol not found",
        ))
    }

    pub(crate) const fn as_ptr(&self) -> *mut r_efi::protocols::device_path::Protocol {
        self.0.as_ptr()
    }

    pub(crate) const fn borrow<'a>(&'a self) -> BorrowedDevicePath<'a> {
        BorrowedDevicePath::new(self.0)
    }
}

impl Drop for OwnedDevicePath {
    fn drop(&mut self) {
        if let Some(bt) = boot_services() {
            let bt: NonNull<r_efi::efi::BootServices> = bt.cast();
            unsafe {
                ((*bt.as_ptr()).free_pool)(self.0.as_ptr() as *mut crate::ffi::c_void);
            }
        }
    }
}

impl crate::fmt::Debug for OwnedDevicePath {
    fn fmt(&self, f: &mut crate::fmt::Formatter<'_>) -> crate::fmt::Result {
        match self.borrow().to_text() {
            Ok(p) => p.fmt(f),
            Err(_) => f.debug_struct("OwnedDevicePath").finish_non_exhaustive(),
        }
    }
}

pub(crate) struct BorrowedDevicePath<'a> {
    protocol: NonNull<r_efi::protocols::device_path::Protocol>,
    phantom: PhantomData<&'a r_efi::protocols::device_path::Protocol>,
}

impl<'a> BorrowedDevicePath<'a> {
    pub(crate) const fn new(protocol: NonNull<r_efi::protocols::device_path::Protocol>) -> Self {
        Self { protocol, phantom: PhantomData }
    }

    pub(crate) fn to_text(&self) -> io::Result<OsString> {
        device_path_to_text(self.protocol)
    }

    pub(crate) const fn iter(&'a self) -> DevicePathIterator<'a> {
        DevicePathIterator::new(DevicePathNode::new(self.protocol))
    }
}

impl<'a> crate::fmt::Debug for BorrowedDevicePath<'a> {
    fn fmt(&self, f: &mut crate::fmt::Formatter<'_>) -> crate::fmt::Result {
        match self.to_text() {
            Ok(p) => p.fmt(f),
            Err(_) => f.debug_struct("BorrowedDevicePath").finish_non_exhaustive(),
        }
    }
}

pub(crate) struct DevicePathIterator<'a>(Option<DevicePathNode<'a>>);

impl<'a> DevicePathIterator<'a> {
    const fn new(node: DevicePathNode<'a>) -> Self {
        if node.is_end() { Self(None) } else { Self(Some(node)) }
    }
}

impl<'a> Iterator for DevicePathIterator<'a> {
    type Item = DevicePathNode<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let cur_node = self.0?;

        let next_node = unsafe { cur_node.next_node() };
        self.0 = if next_node.is_end() { None } else { Some(next_node) };

        Some(cur_node)
    }
}

#[derive(Copy, Clone)]
pub(crate) struct DevicePathNode<'a> {
    protocol: NonNull<r_efi::protocols::device_path::Protocol>,
    phantom: PhantomData<&'a r_efi::protocols::device_path::Protocol>,
}

impl<'a> DevicePathNode<'a> {
    pub(crate) const fn new(protocol: NonNull<r_efi::protocols::device_path::Protocol>) -> Self {
        Self { protocol, phantom: PhantomData }
    }

    pub(crate) const fn length(&self) -> u16 {
        let len = unsafe { (*self.protocol.as_ptr()).length };
        u16::from_le_bytes(len)
    }

    pub(crate) const fn node_type(&self) -> u8 {
        unsafe { (*self.protocol.as_ptr()).r#type }
    }

    pub(crate) const fn sub_type(&self) -> u8 {
        unsafe { (*self.protocol.as_ptr()).sub_type }
    }

    pub(crate) fn data(&self) -> &[u8] {
        let length: usize = self.length().into();

        // Some nodes do not have any special data
        if length > 4 {
            let raw_ptr: *const u8 = self.protocol.as_ptr().cast();
            let data = unsafe { raw_ptr.add(4) };
            unsafe { crate::slice::from_raw_parts(data, length - 4) }
        } else {
            &[]
        }
    }

    pub(crate) const fn is_end(&self) -> bool {
        self.node_type() == r_efi::protocols::device_path::TYPE_END
            && self.sub_type() == r_efi::protocols::device_path::End::SUBTYPE_ENTIRE
    }

    pub(crate) const fn is_end_instance(&self) -> bool {
        self.node_type() == r_efi::protocols::device_path::TYPE_END
            && self.sub_type() == r_efi::protocols::device_path::End::SUBTYPE_INSTANCE
    }

    pub(crate) unsafe fn next_node(&self) -> Self {
        let node = unsafe {
            self.protocol
                .cast::<u8>()
                .add(self.length().into())
                .cast::<r_efi::protocols::device_path::Protocol>()
        };
        Self::new(node)
    }

    pub(crate) fn to_path(&'a self) -> BorrowedDevicePath<'a> {
        BorrowedDevicePath::new(self.protocol)
    }

    pub(crate) fn to_text(&self) -> io::Result<OsString> {
        device_node_to_text(self.protocol)
    }
}

impl<'a> PartialEq for DevicePathNode<'a> {
    fn eq(&self, other: &Self) -> bool {
        // Compare as a single buffer rather than by field since it optimizes better.
        //
        // SAFETY: `Protocol` is followed by a buffer of `length - sizeof::<Protocol>()`. `Protocol`
        // has no padding so it is sound to interpret as a slice.
        unsafe {
            let s1 =
                slice::from_raw_parts(self.protocol.as_ptr().cast::<u8>(), self.length().into());
            let s2 =
                slice::from_raw_parts(other.protocol.as_ptr().cast::<u8>(), other.length().into());
            s1 == s2
        }
    }
}

impl<'a> crate::fmt::Debug for DevicePathNode<'a> {
    fn fmt(&self, f: &mut crate::fmt::Formatter<'_>) -> crate::fmt::Result {
        match self.to_text() {
            Ok(p) => p.fmt(f),
            Err(_) => f
                .debug_struct("DevicePathNode")
                .field("type", &self.node_type())
                .field("sub_type", &self.sub_type())
                .field("length", &self.length())
                .field("specific_device_path_data", &self.data())
                .finish(),
        }
    }
}

pub(crate) struct OwnedProtocol<T> {
    guid: r_efi::efi::Guid,
    handle: NonNull<crate::ffi::c_void>,
    protocol: *mut T,
}

impl<T> OwnedProtocol<T> {
    // FIXME: Consider using unsafe trait for matching protocol with guid
    pub(crate) unsafe fn create(protocol: T, mut guid: r_efi::efi::Guid) -> io::Result<Self> {
        let bt: NonNull<r_efi::efi::BootServices> =
            boot_services().ok_or(BOOT_SERVICES_UNAVAILABLE)?.cast();
        let protocol: *mut T = Box::into_raw(Box::new(protocol));
        let mut handle: r_efi::efi::Handle = crate::ptr::null_mut();

        // FIXME: Move into r-efi once extended_varargs_abi_support is stabilized
        let func: BootInstallMultipleProtocolInterfaces =
            unsafe { crate::mem::transmute((*bt.as_ptr()).install_multiple_protocol_interfaces) };

        let r = unsafe {
            func(
                &mut handle,
                &mut guid as *mut _ as *mut crate::ffi::c_void,
                protocol as *mut crate::ffi::c_void,
                crate::ptr::null_mut() as *mut crate::ffi::c_void,
            )
        };

        if r.is_error() {
            drop(unsafe { Box::from_raw(protocol) });
            return Err(crate::io::Error::from_raw_os_error(r.as_usize()));
        };

        let handle = NonNull::new(handle)
            .ok_or(io::const_error!(io::ErrorKind::Uncategorized, "found null handle"))?;

        Ok(Self { guid, handle, protocol })
    }

    pub(crate) fn handle(&self) -> NonNull<crate::ffi::c_void> {
        self.handle
    }
}

impl<T> Drop for OwnedProtocol<T> {
    fn drop(&mut self) {
        // Do not deallocate a runtime protocol
        if let Some(bt) = boot_services() {
            let bt: NonNull<r_efi::efi::BootServices> = bt.cast();
            // FIXME: Move into r-efi once extended_varargs_abi_support is stabilized
            let func: BootUninstallMultipleProtocolInterfaces = unsafe {
                crate::mem::transmute((*bt.as_ptr()).uninstall_multiple_protocol_interfaces)
            };
            let status = unsafe {
                func(
                    self.handle.as_ptr(),
                    &mut self.guid as *mut _ as *mut crate::ffi::c_void,
                    self.protocol as *mut crate::ffi::c_void,
                    crate::ptr::null_mut() as *mut crate::ffi::c_void,
                )
            };

            // Leak the protocol in case uninstall fails
            if status == r_efi::efi::Status::SUCCESS {
                let _ = unsafe { Box::from_raw(self.protocol) };
            }
        }
    }
}

impl<T> AsRef<T> for OwnedProtocol<T> {
    fn as_ref(&self) -> &T {
        unsafe { self.protocol.as_ref().unwrap() }
    }
}

pub(crate) struct OwnedTable<T> {
    layout: crate::alloc::Layout,
    ptr: *mut T,
}

impl<T> OwnedTable<T> {
    pub(crate) fn from_table_header(hdr: &r_efi::efi::TableHeader) -> Self {
        let header_size = hdr.header_size as usize;
        let layout = crate::alloc::Layout::from_size_align(header_size, 8).unwrap();
        let ptr = unsafe { crate::alloc::alloc(layout) as *mut T };
        Self { layout, ptr }
    }

    pub(crate) const fn as_ptr(&self) -> *const T {
        self.ptr
    }

    pub(crate) const fn as_mut_ptr(&self) -> *mut T {
        self.ptr
    }
}

impl OwnedTable<r_efi::efi::SystemTable> {
    pub(crate) fn from_table(tbl: *const r_efi::efi::SystemTable) -> Self {
        let hdr = unsafe { (*tbl).hdr };

        let owned_tbl = Self::from_table_header(&hdr);
        unsafe {
            crate::ptr::copy_nonoverlapping(
                tbl as *const u8,
                owned_tbl.as_mut_ptr() as *mut u8,
                hdr.header_size as usize,
            )
        };

        owned_tbl
    }
}

impl<T> Drop for OwnedTable<T> {
    fn drop(&mut self) {
        unsafe { crate::alloc::dealloc(self.ptr as *mut u8, self.layout) };
    }
}

/// Create OsString from a pointer to NULL terminated UTF-16 string
pub(crate) fn os_string_from_raw(ptr: *mut r_efi::efi::Char16) -> Option<OsString> {
    let path_len = unsafe { WStrUnits::new(ptr)?.count() };
    Some(OsString::from_wide(unsafe { slice::from_raw_parts(ptr.cast(), path_len) }))
}

/// Create NULL terminated UTF-16 string
pub(crate) fn os_string_to_raw(s: &OsStr) -> Option<Box<[r_efi::efi::Char16]>> {
    let temp = s.encode_wide().chain(Some(0)).collect::<Box<[r_efi::efi::Char16]>>();
    if temp[..temp.len() - 1].contains(&0) { None } else { Some(temp) }
}

pub(crate) fn open_shell() -> Option<NonNull<shell::Protocol>> {
    static LAST_VALID_HANDLE: Atomic<*mut crate::ffi::c_void> =
        AtomicPtr::new(crate::ptr::null_mut());

    if let Some(handle) = NonNull::new(LAST_VALID_HANDLE.load(Ordering::Acquire)) {
        if let Ok(protocol) = open_protocol::<shell::Protocol>(handle, shell::PROTOCOL_GUID) {
            return Some(protocol);
        }
    }

    let handles = locate_handles(shell::PROTOCOL_GUID).ok()?;
    for handle in handles {
        if let Ok(protocol) = open_protocol::<shell::Protocol>(handle, shell::PROTOCOL_GUID) {
            LAST_VALID_HANDLE.store(handle.as_ptr(), Ordering::Release);
            return Some(protocol);
        }
    }

    None
}

/// Get device path protocol associated with shell mapping.
///
/// returns None in case no such mapping is exists
pub(crate) fn get_device_path_from_map(map: &Path) -> io::Result<BorrowedDevicePath<'static>> {
    let shell =
        open_shell().ok_or(io::const_error!(io::ErrorKind::NotFound, "UEFI Shell not found"))?;
    let mut path = os_string_to_raw(map.as_os_str())
        .ok_or(io::const_error!(io::ErrorKind::InvalidFilename, "invalid UEFI shell mapping"))?;

    // The Device Path Protocol pointer returned by UEFI shell is owned by the shell and is not
    // freed throughout it's lifetime. So it has a 'static lifetime.
    let protocol = unsafe { ((*shell.as_ptr()).get_device_path_from_map)(path.as_mut_ptr()) };
    let protocol = NonNull::new(protocol)
        .ok_or(io::const_error!(io::ErrorKind::NotFound, "UEFI Shell mapping not found"))?;

    Ok(BorrowedDevicePath::new(protocol))
}

/// Helper for UEFI Protocols which are created and destroyed using
/// [EFI_SERVICE_BINDING_PROTOCOL](https://uefi.org/specs/UEFI/2.11/11_Protocols_UEFI_Driver_Model.html#efi-service-binding-protocol)
pub(crate) struct ServiceProtocol {
    service_guid: r_efi::efi::Guid,
    handle: NonNull<crate::ffi::c_void>,
    child_handle: NonNull<crate::ffi::c_void>,
}

impl ServiceProtocol {
    pub(crate) fn open(service_guid: r_efi::efi::Guid) -> io::Result<Self> {
        let handles = locate_handles(service_guid)?;

        for handle in handles {
            if let Ok(protocol) = open_protocol::<service_binding::Protocol>(handle, service_guid) {
                let Ok(child_handle) = Self::create_child(protocol) else {
                    continue;
                };

                return Ok(Self { service_guid, handle, child_handle });
            }
        }

        Err(io::const_error!(io::ErrorKind::NotFound, "no service binding protocol found"))
    }

    pub(crate) fn child_handle(&self) -> NonNull<crate::ffi::c_void> {
        self.child_handle
    }

    fn create_child(
        sbp: NonNull<service_binding::Protocol>,
    ) -> io::Result<NonNull<crate::ffi::c_void>> {
        let mut child_handle: r_efi::efi::Handle = crate::ptr::null_mut();
        // SAFETY: A new handle is allocated if a pointer to NULL is passed.
        let r = unsafe { ((*sbp.as_ptr()).create_child)(sbp.as_ptr(), &mut child_handle) };

        if r.is_error() {
            Err(crate::io::Error::from_raw_os_error(r.as_usize()))
        } else {
            NonNull::new(child_handle)
                .ok_or(const_error!(io::ErrorKind::Other, "null child handle"))
        }
    }
}

impl Drop for ServiceProtocol {
    fn drop(&mut self) {
        if let Ok(sbp) = open_protocol::<service_binding::Protocol>(self.handle, self.service_guid)
        {
            // SAFETY: Child handle must be allocated by the current service binding protocol.
            let _ = unsafe {
                ((*sbp.as_ptr()).destroy_child)(sbp.as_ptr(), self.child_handle.as_ptr())
            };
        }
    }
}

#[repr(transparent)]
pub(crate) struct OwnedEvent(NonNull<crate::ffi::c_void>);

impl OwnedEvent {
    pub(crate) fn new(
        signal: u32,
        tpl: efi::Tpl,
        handler: Option<efi::EventNotify>,
        context: Option<NonNull<crate::ffi::c_void>>,
    ) -> io::Result<Self> {
        let boot_services: NonNull<efi::BootServices> =
            boot_services().ok_or(BOOT_SERVICES_UNAVAILABLE)?.cast();
        let mut event: r_efi::efi::Event = crate::ptr::null_mut();
        let context = context.map(NonNull::as_ptr).unwrap_or(crate::ptr::null_mut());

        let r = unsafe {
            let create_event = (*boot_services.as_ptr()).create_event;
            (create_event)(signal, tpl, handler, context, &mut event)
        };

        if r.is_error() {
            Err(crate::io::Error::from_raw_os_error(r.as_usize()))
        } else {
            NonNull::new(event)
                .ok_or(const_error!(io::ErrorKind::Other, "failed to create event"))
                .map(Self)
        }
    }

    pub(crate) fn as_ptr(&self) -> efi::Event {
        self.0.as_ptr()
    }

    pub(crate) fn into_raw(self) -> *mut crate::ffi::c_void {
        let r = self.0.as_ptr();
        crate::mem::forget(self);
        r
    }

    /// SAFETY: Assumes that ptr is a non-null valid UEFI event
    pub(crate) unsafe fn from_raw(ptr: *mut crate::ffi::c_void) -> Self {
        Self(unsafe { NonNull::new_unchecked(ptr) })
    }
}

impl Drop for OwnedEvent {
    fn drop(&mut self) {
        if let Some(boot_services) = boot_services() {
            let bt: NonNull<r_efi::efi::BootServices> = boot_services.cast();
            unsafe {
                let close_event = (*bt.as_ptr()).close_event;
                (close_event)(self.0.as_ptr())
            };
        }
    }
}

pub(crate) const fn ipv4_to_r_efi(addr: crate::net::Ipv4Addr) -> efi::Ipv4Address {
    efi::Ipv4Address { addr: addr.octets() }
}

pub(crate) const fn ipv4_from_r_efi(ip: efi::Ipv4Address) -> crate::net::Ipv4Addr {
    crate::net::Ipv4Addr::new(ip.addr[0], ip.addr[1], ip.addr[2], ip.addr[3])
}
