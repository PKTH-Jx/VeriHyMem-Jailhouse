//! Executable adapter between Jailhouse and VeriHyMem.
//!
//! The initial integration owns one concrete AArch64 stage-2 page table. Jailhouse
//! supplies a dedicated HVA-backed frame pool and its fixed HVA-to-PA offset;
//! VeriHyMem's `GlobalFrameAllocator` owns frame allocation and all table mutations
//! inside that pool. Rust heap allocation is a separate, unverified Jailhouse hook.
#![cfg_attr(not(test), no_std)]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod heap;

use alloc::{boxed::Box, vec};
#[cfg(not(test))]
use core::panic::PanicInfo;
use core::ptr;
use spin::Once;
use verified_hv_mem::address::addr::{PAddr, VAddr};
use verified_hv_mem::address::frame::{Frame, FrameSize, MemAttr};
use verified_hv_mem::bitmap_allocator::bitmap_impl::BitAlloc256;
use verified_hv_mem::global_allocator::GlobalAllocator;
use verified_hv_mem::page_table::pt_arch::{PTArch, PTArchLevel};
use verified_hv_mem::page_table::{Aarch64PTE, ExPageTable, PTConstants, PageTable};
use vstd::prelude::Tracked;

pub const PAGE_SIZE: usize = 0x1000;
pub const THREE_LEVEL_IPA_BITS: u8 = 39;
pub const FOUR_LEVEL_MIN_IPA_BITS: u8 = 44;
pub const MAX_IPA_BITS: u8 = 48;
pub const MAX_PA: usize = (1usize << MAX_IPA_BITS) - 1;

const BIT_ALLOC_CAPACITY: usize = 1 << 8;
const BIT_ALLOC_ADDRESS_SPAN: usize = BIT_ALLOC_CAPACITY * PAGE_SIZE;

#[derive(Clone, Copy)]
struct GlobalFramePoolConfig {
    hva_base: usize,
    frame_count: usize,
    hva_to_pa_offset: usize,
}

/// VeriHyMem's verified global frame allocator specialization.
///
/// This is distinct from the Rust global heap allocator in `heap.rs`.
pub type GlobalFrameAllocator = GlobalAllocator<BitAlloc256>;

// Jailhouse's bootstrap mapping reaches initialized data before the final
// hypervisor mappings are installed. Keep these early-init singletons there
// instead of allowing zero initialization to place them at the end of .bss.
#[cfg_attr(not(test), unsafe(link_section = ".data"))]
static GLOBAL_FRAME_POOL_CONFIG: Once<GlobalFramePoolConfig> = Once::new();
#[cfg_attr(not(test), unsafe(link_section = ".data"))]
static GLOBAL_FRAME_ALLOCATOR: Once<GlobalFrameAllocator> = Once::new();

pub type ConcretePageTable = ExPageTable<BitAlloc256, Aarch64PTE>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum Error {
    InvalidArgument = -22,
    NotMapped = -2,
    AlreadyMapped = -17,
    Busy = -16,
}

/// Initialize VeriHyMem's global frame allocator from its dedicated Jailhouse
/// frame pool and return the singleton used by all page-table handles.
///
/// The first call fixes the pool geometry. Later calls must describe the same
/// pool; this permits independent page-table objects to share allocator state
/// without wrapping the allocator into each object.
///
/// This prototype assumes the pool never exhausts. VeriHyMem's frame allocation
/// interface is intentionally infallible; exhausting the pool is outside the
/// integration proof boundary and may abort the hypervisor.
unsafe fn init_global_frame_allocator(
    frame_pool_hva_base: usize,
    frame_pool_frame_count: usize,
    hva_to_pa_offset: usize,
) -> Result<&'static GlobalFrameAllocator, Error> {
    let frame_pool = GLOBAL_FRAME_POOL_CONFIG.call_once(|| GlobalFramePoolConfig {
        hva_base: frame_pool_hva_base,
        frame_count: frame_pool_frame_count,
        hva_to_pa_offset,
    });
    if frame_pool.hva_base != frame_pool_hva_base
        || frame_pool.frame_count != frame_pool_frame_count
        || frame_pool.hva_to_pa_offset != hva_to_pa_offset
    {
        return Err(Error::Busy);
    }

    Ok(GLOBAL_FRAME_ALLOCATOR.call_once(|| {
        let frame_pool_bytes = frame_pool.frame_count * PAGE_SIZE;
        unsafe {
            ptr::write_bytes(frame_pool.hva_base as *mut u8, 0, frame_pool_bytes);
        }
        let frame_allocator = GlobalFrameAllocator::default(PAddr(frame_pool.hva_base));

        // This is the trusted handoff from Jailhouse's dedicated frame pool into
        // VeriHyMem. Conditional on this permission matching the concrete pool,
        // GlobalFrameAllocator's verified client-disjointness applies afterwards.
        frame_allocator.init(frame_pool.frame_count, Tracked::assume_new());
        frame_allocator
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapAttrs {
    pub readable: bool,
    pub writable: bool,
    pub executable: bool,
    pub device: bool,
}

impl MapAttrs {
    pub const fn normal(readable: bool, writable: bool, executable: bool) -> Self {
        Self {
            readable,
            writable,
            executable,
            device: false,
        }
    }

    fn into_mem_attr(self) -> MemAttr {
        // Stage-2 mappings are guest-accessible by definition. The current
        // AArch64 PTE backend does not encode execute permission; it is retained
        // only in the input model and is not recovered by a later PTE query.
        MemAttr::new(
            self.readable,
            self.writable,
            self.executable,
            true,
            self.device,
        )
    }
}

/// C wire representation of [`MapAttrs`].
///
/// Rust `bool` only admits the bit patterns 0 and 1, so the FFI accepts bytes
/// and validates them before constructing the internal representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct CMapAttrs {
    pub readable: u8,
    pub writable: u8,
    pub executable: u8,
    pub device: u8,
}

impl TryFrom<CMapAttrs> for MapAttrs {
    type Error = Error;

    fn try_from(attrs: CMapAttrs) -> Result<Self, Self::Error> {
        fn flag(value: u8) -> Result<bool, Error> {
            match value {
                0 => Ok(false),
                1 => Ok(true),
                _ => Err(Error::InvalidArgument),
            }
        }

        Ok(Self {
            readable: flag(attrs.readable)?,
            writable: flag(attrs.writable)?,
            executable: flag(attrs.executable)?,
            device: flag(attrs.device)?,
        })
    }
}

impl From<MapAttrs> for CMapAttrs {
    fn from(attrs: MapAttrs) -> Self {
        Self {
            readable: attrs.readable as u8,
            writable: attrs.writable as u8,
            executable: attrs.executable as u8,
            device: attrs.device as u8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub ipa_base: usize,
    pub pa_base: usize,
    pub size: usize,
    pub attrs: MapAttrs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct CMapping {
    pub ipa_base: usize,
    pub pa_base: usize,
    pub size: usize,
    pub attrs: CMapAttrs,
}

impl From<Mapping> for CMapping {
    fn from(mapping: Mapping) -> Self {
        Self {
            ipa_base: mapping.ipa_base,
            pa_base: mapping.pa_base,
            size: mapping.size,
            attrs: mapping.attrs.into(),
        }
    }
}

/// One executable VeriHyMem page table backed by the dedicated global frame pool.
pub struct JailhousePageTable {
    page_table: ConcretePageTable,
    ipa_bits: u8,
    mapped_pages: usize,
}

impl JailhousePageTable {
    /// Construct an empty AArch64 stage-2 page table.
    ///
    /// # Safety
    ///
    /// `frame_pool_hva_base..frame_pool_hva_base + frame_pool_frame_count * PAGE_SIZE`
    /// must be the valid, exclusively assigned, writable pool represented by the
    /// tracked permission assumed during global frame allocator initialization.
    pub unsafe fn new(
        frame_pool_hva_base: usize,
        frame_pool_frame_count: usize,
        hva_to_pa_offset: usize,
        ipa_bits: u8,
    ) -> Result<Self, Error> {
        validate_global_frame_pool(
            frame_pool_hva_base,
            frame_pool_frame_count,
            hva_to_pa_offset,
            ipa_bits,
        )?;

        let frame_allocator = unsafe {
            init_global_frame_allocator(
                frame_pool_hva_base,
                frame_pool_frame_count,
                hva_to_pa_offset,
            )?
        };

        let mut levels = vec![];
        if ipa_bits >= FOUR_LEVEL_MIN_IPA_BITS {
            levels.push(PTArchLevel {
                entry_count: 512,
                frame_size: FrameSize::Size512G,
            });
        }
        levels.extend([
            PTArchLevel {
                entry_count: 512,
                frame_size: FrameSize::Size1G,
            },
            PTArchLevel {
                entry_count: 512,
                frame_size: FrameSize::Size2M,
            },
            PTArchLevel {
                entry_count: 512,
                frame_size: FrameSize::Size4K,
            },
        ]);
        let arch = PTArch(levels);
        let constants = PTConstants {
            arch,
            hva_to_pa_offset,
        };
        let page_table = ConcretePageTable::new(frame_allocator, constants);

        Ok(Self {
            page_table,
            ipa_bits,
            mapped_pages: 0,
        })
    }

    pub fn ipa_bits(&self) -> u8 {
        self.ipa_bits
    }

    pub fn mapped_pages(&self) -> usize {
        self.mapped_pages
    }

    /// Physical address to install in the stage-2 root register.
    pub fn root_pa(&self) -> usize {
        self.page_table.root().0
    }

    pub fn map_page(&mut self, ipa: usize, pa: usize, attrs: MapAttrs) -> Result<(), Error> {
        self.validate_ipa_page(ipa)?;
        validate_pa_page(pa)?;

        let frame = Frame {
            base: PAddr(pa),
            size: FrameSize::Size4K,
            attr: attrs.into_mem_attr(),
        };
        let Some(frame_allocator) = GLOBAL_FRAME_ALLOCATOR.get() else {
            return Err(Error::Busy);
        };
        self.page_table
            .map(frame_allocator, VAddr(ipa), frame)
            .map_err(|_| Error::AlreadyMapped)?;
        self.mapped_pages += 1;
        Ok(())
    }

    pub fn unmap_page(&mut self, ipa: usize) -> Result<(), Error> {
        self.validate_ipa_page(ipa)?;
        let Some(frame_allocator) = GLOBAL_FRAME_ALLOCATOR.get() else {
            return Err(Error::Busy);
        };
        self.page_table
            .unmap(frame_allocator, VAddr(ipa))
            .map_err(|_| Error::NotMapped)?;
        self.mapped_pages -= 1;
        Ok(())
    }

    pub fn query(&self, ipa: usize) -> Result<Mapping, Error> {
        self.validate_ipa(ipa)?;
        let (ipa_base, frame) = self
            .page_table
            .query(VAddr(ipa))
            .map_err(|_| Error::NotMapped)?;
        Ok(Mapping {
            ipa_base: ipa_base.0,
            pa_base: frame.base.0,
            size: frame.size.as_usize(),
            attrs: MapAttrs {
                readable: frame.attr.readable,
                writable: frame.attr.writable,
                executable: frame.attr.executable,
                device: frame.attr.device,
            },
        })
    }

    /// Destroy an empty page table and return its root frame to the dedicated pool.
    ///
    /// If mappings remain, ownership of `self` is returned to the caller.
    pub fn destroy_empty(self: Box<Self>) -> Result<(), Box<Self>> {
        if self.mapped_pages != 0 {
            return Err(self);
        }
        let Some(frame_allocator) = GLOBAL_FRAME_ALLOCATOR.get() else {
            return Err(self);
        };
        let Self { page_table, .. } = *self;
        page_table.drop(frame_allocator);
        Ok(())
    }

    fn validate_ipa(&self, ipa: usize) -> Result<(), Error> {
        let limit = 1usize << self.ipa_bits;
        if ipa >= limit {
            return Err(Error::InvalidArgument);
        }
        Ok(())
    }

    fn validate_ipa_page(&self, ipa: usize) -> Result<(), Error> {
        self.validate_ipa(ipa)?;
        if ipa % PAGE_SIZE != 0 {
            return Err(Error::InvalidArgument);
        }
        Ok(())
    }
}

fn validate_global_frame_pool(
    frame_pool_hva_base: usize,
    frame_pool_frame_count: usize,
    hva_to_pa_offset: usize,
    ipa_bits: u8,
) -> Result<(), Error> {
    if frame_pool_hva_base % PAGE_SIZE != 0
        || hva_to_pa_offset % PAGE_SIZE != 0
        || hva_to_pa_offset > frame_pool_hva_base
        || frame_pool_frame_count == 0
        || frame_pool_frame_count > BIT_ALLOC_CAPACITY
        || !(ipa_bits == THREE_LEVEL_IPA_BITS
            || (FOUR_LEVEL_MIN_IPA_BITS..=MAX_IPA_BITS).contains(&ipa_bits))
    {
        return Err(Error::InvalidArgument);
    }

    let frame_pool_bytes = frame_pool_frame_count
        .checked_mul(PAGE_SIZE)
        .ok_or(Error::InvalidArgument)?;
    frame_pool_hva_base
        .checked_add(frame_pool_bytes)
        .ok_or(Error::InvalidArgument)?;
    frame_pool_hva_base
        .checked_add(BIT_ALLOC_ADDRESS_SPAN)
        .ok_or(Error::InvalidArgument)?;

    let frame_pool_pa_base = frame_pool_hva_base - hva_to_pa_offset;
    let frame_pool_pa_end = frame_pool_pa_base
        .checked_add(frame_pool_bytes)
        .ok_or(Error::InvalidArgument)?;
    if frame_pool_pa_end == 0 || frame_pool_pa_end - 1 > MAX_PA {
        return Err(Error::InvalidArgument);
    }
    Ok(())
}

fn validate_pa_page(pa: usize) -> Result<(), Error> {
    if pa % PAGE_SIZE != 0 || pa > MAX_PA - (PAGE_SIZE - 1) {
        return Err(Error::InvalidArgument);
    }
    Ok(())
}

/// Initialize the global frame allocator from its dedicated frame pool.
/// The page-table create entry point performs the same initialization lazily,
/// so Jailhouse may call this explicitly during paging setup or omit it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vj_global_frame_allocator_init(
    frame_pool_hva_base: usize,
    frame_pool_frame_count: usize,
    hva_to_pa_offset: usize,
) -> i32 {
    if validate_global_frame_pool(
        frame_pool_hva_base,
        frame_pool_frame_count,
        hva_to_pa_offset,
        THREE_LEVEL_IPA_BITS,
    )
    .is_err()
    {
        return Error::InvalidArgument as i32;
    }
    match unsafe {
        init_global_frame_allocator(
            frame_pool_hva_base,
            frame_pool_frame_count,
            hva_to_pa_offset,
        )
    } {
        Ok(_) => 0,
        Err(err) => err as i32,
    }
}

/// Allocate and initialize one opaque VeriHyMem page-table instance.
///
/// `out_handle` receives the opaque handle on success and is set to null on
/// failure. The global frame pool is exclusively assigned by Jailhouse; this
/// handle records only its page-table client state.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vj_pt_create(
    frame_pool_hva_base: usize,
    frame_pool_frame_count: usize,
    hva_to_pa_offset: usize,
    ipa_bits: u8,
    out_handle: *mut *mut JailhousePageTable,
) -> i32 {
    let Some(out_handle) = (unsafe { out_handle.as_mut() }) else {
        return Error::InvalidArgument as i32;
    };
    *out_handle = core::ptr::null_mut();

    match unsafe {
        JailhousePageTable::new(
            frame_pool_hva_base,
            frame_pool_frame_count,
            hva_to_pa_offset,
            ipa_bits,
        )
    } {
        Ok(table) => {
            *out_handle = Box::into_raw(Box::new(table));
            0
        }
        Err(err) => err as i32,
    }
}

/// Map one 4 KiB page into an opaque page-table instance.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vj_pt_map_page(
    handle: *mut JailhousePageTable,
    ipa: usize,
    pa: usize,
    attrs: CMapAttrs,
) -> i32 {
    let Some(table) = (unsafe { handle.as_mut() }) else {
        return Error::InvalidArgument as i32;
    };
    let Ok(attrs) = MapAttrs::try_from(attrs) else {
        return Error::InvalidArgument as i32;
    };
    match table.map_page(ipa, pa, attrs) {
        Ok(()) => 0,
        Err(err) => err as i32,
    }
}

/// Unmap one 4 KiB page from an opaque page-table instance.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vj_pt_unmap_page(handle: *mut JailhousePageTable, ipa: usize) -> i32 {
    let Some(table) = (unsafe { handle.as_mut() }) else {
        return Error::InvalidArgument as i32;
    };
    match table.unmap_page(ipa) {
        Ok(()) => 0,
        Err(err) => err as i32,
    }
}

/// Query one mapping. `out` must point to writable caller-owned storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vj_pt_query(
    handle: *const JailhousePageTable,
    ipa: usize,
    out: *mut CMapping,
) -> i32 {
    let (Some(table), Some(out)) = (unsafe { handle.as_ref() }, unsafe { out.as_mut() }) else {
        return Error::InvalidArgument as i32;
    };
    match table.query(ipa) {
        Ok(mapping) => {
            *out = mapping.into();
            0
        }
        Err(err) => err as i32,
    }
}

/// Return the page-table root physical address used by VTTBR_EL2.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vj_pt_root_pa(
    handle: *const JailhousePageTable,
    out_root_pa: *mut usize,
) -> i32 {
    let (Some(table), Some(out_root_pa)) =
        (unsafe { handle.as_ref() }, unsafe { out_root_pa.as_mut() })
    else {
        return Error::InvalidArgument as i32;
    };
    *out_root_pa = table.root_pa();
    0
}

/// Return the number of 4 KiB mappings owned by this handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vj_pt_mapped_pages(
    handle: *const JailhousePageTable,
    out_mapped_pages: *mut usize,
) -> i32 {
    let (Some(table), Some(out_mapped_pages)) = (unsafe { handle.as_ref() }, unsafe {
        out_mapped_pages.as_mut()
    }) else {
        return Error::InvalidArgument as i32;
    };
    *out_mapped_pages = table.mapped_pages();
    0
}

/// Destroy an empty page-table instance and release its table frames.
/// Returns `Busy` if mappings remain and leaves the original handle valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vj_pt_destroy(handle: *mut JailhousePageTable) -> i32 {
    if handle.is_null() {
        return Error::InvalidArgument as i32;
    }
    let table = unsafe { Box::from_raw(handle) };
    match table.destroy_empty() {
        Ok(()) => 0,
        Err(table) => {
            let restored_handle = Box::into_raw(table);
            debug_assert_eq!(restored_handle, handle);
            Error::Busy as i32
        }
    }
}

unsafe extern "C" {
    fn vj_abort() -> !;
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    unsafe { vj_abort() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::MaybeUninit;
    use std::alloc::{Layout, alloc_zeroed};

    #[test]
    fn c_abi_page_table_lifecycle_and_busy_destroy() {
        const FRAME_COUNT: usize = 64;
        let layout = Layout::from_size_align(FRAME_COUNT * PAGE_SIZE, PAGE_SIZE).unwrap();
        let frame_pool = unsafe { alloc_zeroed(layout) };
        assert!(!frame_pool.is_null());

        // GLOBAL_FRAME_ALLOCATOR retains this dedicated pool for process lifetime.
        // Leaking it here models Jailhouse's static-lifetime frame-pool handoff.
        let frame_pool_hva_base = frame_pool as usize;
        let mut handle = core::ptr::null_mut();

        assert_eq!(
            unsafe {
                vj_pt_create(
                    frame_pool_hva_base,
                    FRAME_COUNT,
                    0,
                    THREE_LEVEL_IPA_BITS,
                    &mut handle,
                )
            },
            0,
        );
        assert!(!handle.is_null());

        assert_eq!(
            unsafe {
                vj_pt_map_page(
                    handle,
                    1usize << THREE_LEVEL_IPA_BITS,
                    0x8000,
                    CMapAttrs {
                        readable: 1,
                        writable: 1,
                        executable: 0,
                        device: 0,
                    },
                )
            },
            Error::InvalidArgument as i32,
        );

        let mut root_pa = 0;
        assert_eq!(unsafe { vj_pt_root_pa(handle, &mut root_pa) }, 0);
        assert!(root_pa >= frame_pool_hva_base);
        assert!(root_pa < frame_pool_hva_base + FRAME_COUNT * PAGE_SIZE);
        assert_eq!(root_pa % PAGE_SIZE, 0);

        let invalid_attrs = CMapAttrs {
            readable: 2,
            writable: 1,
            executable: 0,
            device: 0,
        };
        assert_eq!(
            unsafe { vj_pt_map_page(handle, 0x4000, 0x8000, invalid_attrs) },
            Error::InvalidArgument as i32,
        );

        let attrs = CMapAttrs {
            readable: 1,
            writable: 1,
            executable: 0,
            device: 0,
        };
        assert_eq!(unsafe { vj_pt_map_page(handle, 0x4000, 0x8000, attrs) }, 0,);

        let mut mapped_pages = 0;
        assert_eq!(unsafe { vj_pt_mapped_pages(handle, &mut mapped_pages) }, 0,);
        assert_eq!(mapped_pages, 1);

        let mut mapping = MaybeUninit::<CMapping>::uninit();
        assert_eq!(
            unsafe { vj_pt_query(handle, 0x4000, mapping.as_mut_ptr()) },
            0,
        );
        let mapping = unsafe { mapping.assume_init() };
        assert_eq!(mapping.ipa_base, 0x4000);
        assert_eq!(mapping.pa_base, 0x8000);
        assert_eq!(mapping.size, PAGE_SIZE);
        assert_eq!(mapping.attrs.readable, 1);
        assert_eq!(mapping.attrs.writable, 1);

        assert_eq!(unsafe { vj_pt_destroy(handle) }, Error::Busy as i32,);
        let mut mapping_after_busy = MaybeUninit::<CMapping>::uninit();
        assert_eq!(
            unsafe { vj_pt_query(handle, 0x4000, mapping_after_busy.as_mut_ptr()) },
            0,
        );

        // Busy destruction preserves the original handle, so it remains usable.
        assert_eq!(unsafe { vj_pt_unmap_page(handle, 0x4000) }, 0);
        assert_eq!(unsafe { vj_pt_destroy(handle) }, 0);
    }
}
