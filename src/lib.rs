#![no_std]

//! Executable adapter between Jailhouse and VeriHyMem.
//!
//! The initial integration owns one concrete AArch64 stage-2 page table. Jailhouse
//! supplies an HVA-backed memory pool and its fixed HVA-to-PA offset; VeriHyMem owns
//! allocation and all table mutations inside that pool.

extern crate alloc;

mod heap;

use alloc::{boxed::Box, vec};
use core::panic::PanicInfo;
use core::ptr;
use verified_hv_mem::address::addr::{PAddr, VAddr};
use verified_hv_mem::address::frame::{Frame, FrameSize, MemAttr};
use verified_hv_mem::bitmap_allocator::bitmap_impl::BitAlloc1M;
use verified_hv_mem::global_allocator::GlobalAllocator;
use verified_hv_mem::page_table::pt_arch::{PTArch, PTArchLevel};
use verified_hv_mem::page_table::{Aarch64PTE, ExPageTable, PTConstants, PageTable};
use vstd::prelude::Tracked;
use spin::Once;

pub const PAGE_SIZE: usize = 0x1000;
pub const MIN_IPA_BITS: u8 = 44;
pub const MAX_IPA_BITS: u8 = 48;
pub const MAX_PA: usize = (1usize << MAX_IPA_BITS) - 1;

const BIT_ALLOC_CAPACITY: usize = 1 << 20;
const BIT_ALLOC_ADDRESS_SPAN: usize = BIT_ALLOC_CAPACITY * PAGE_SIZE;

#[derive(Clone, Copy)]
struct MemPoolConfig {
    hva_base: usize,
    frame_count: usize,
    hva_to_pa_offset: usize,
}

static MEM_POOL: Once<MemPoolConfig> = Once::new();
static GLOBAL_ALLOCATOR: Once<GlobalAllocator<BitAlloc1M>> = Once::new();

pub type ConcretePageTable = ExPageTable<BitAlloc1M, Aarch64PTE>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum Error {
    InvalidArgument = -22,
    NotMapped = -2,
    AlreadyMapped = -17,
    Busy = -16,
}

/// Initialize the shared VeriHyMem allocator from Jailhouse's reserved memory
/// pool and return the singleton allocator used by all page-table handles.
///
/// The first call fixes the pool geometry. Later calls must describe the same
/// pool; this permits independent page-table objects to share allocator state
/// without wrapping the allocator into each object.
unsafe fn init_global_allocator(
    table_hva_base: usize,
    table_frame_count: usize,
    hva_to_pa_offset: usize,
) -> Result<&'static GlobalAllocator<BitAlloc1M>, Error> {
    let mem_pool = MEM_POOL.call_once(|| MemPoolConfig {
        hva_base: table_hva_base,
        frame_count: table_frame_count,
        hva_to_pa_offset,
    });
    if mem_pool.hva_base != table_hva_base
        || mem_pool.frame_count != table_frame_count
        || mem_pool.hva_to_pa_offset != hva_to_pa_offset
    {
        return Err(Error::Busy);
    }

    Ok(GLOBAL_ALLOCATOR.call_once(|| {
        let table_bytes = mem_pool.frame_count * PAGE_SIZE;
        unsafe {
            ptr::write_bytes(mem_pool.hva_base as *mut u8, 0, table_bytes);
        }
        let allocator = GlobalAllocator::<BitAlloc1M>::default(PAddr(mem_pool.hva_base));

        // Frame permissions are proof-only. Jailhouse establishes the executable
        // ownership of the pool; the integration intentionally omits tracked
        // permission construction.
        allocator.init(mem_pool.frame_count, Tracked::assume_new());
        allocator
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct MapAttrs {
    pub readable: bool,
    pub writable: bool,
    pub executable: bool,
    pub device: bool,
}

impl MapAttrs {
    pub const fn normal(readable: bool, writable: bool, executable: bool) -> Self {
        Self { readable, writable, executable, device: false }
    }

    fn into_mem_attr(self) -> MemAttr {
        // Stage-2 mappings are guest-accessible by definition. The current
        // AArch64 PTE backend does not encode execute permission; the value is
        // retained here so the wrapper API does not lose Jailhouse information.
        MemAttr::new(self.readable, self.writable, self.executable, true, self.device)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Mapping {
    pub ipa_base: usize,
    pub pa_base: usize,
    pub size: usize,
    pub attrs: MapAttrs,
}

/// One executable VeriHyMem page table backed by a Jailhouse-owned HVA memory pool.
pub struct JailhousePageTable {
    page_table: ConcretePageTable,
    ipa_bits: u8,
    mapped_pages: usize,
}

impl JailhousePageTable {
    /// Construct an empty four-level AArch64 stage-2 page table.
    ///
    /// # Safety
    ///
    /// `table_hva_base..table_hva_base + table_frame_count * PAGE_SIZE` must be
    /// valid, exclusively owned, writable hypervisor virtual memory for the
    /// lifetime of the returned value. The pool must not be used by Jailhouse's
    /// native page allocator while this value exists.
    pub unsafe fn new(
        table_hva_base: usize,
        table_frame_count: usize,
        hva_to_pa_offset: usize,
        ipa_bits: u8,
    ) -> Result<Self, Error> {
        validate_mem_pool(table_hva_base, table_frame_count, hva_to_pa_offset, ipa_bits)?;

        let allocator = unsafe {
            init_global_allocator(table_hva_base, table_frame_count, hva_to_pa_offset)?
        };

        let arch = PTArch(vec![
            PTArchLevel { entry_count: 512, frame_size: FrameSize::Size512G },
            PTArchLevel { entry_count: 512, frame_size: FrameSize::Size1G },
            PTArchLevel { entry_count: 512, frame_size: FrameSize::Size2M },
            PTArchLevel { entry_count: 512, frame_size: FrameSize::Size4K },
        ]);
        let constants = PTConstants { arch, hva_to_pa_offset };
        let page_table = ConcretePageTable::new(allocator, constants);

        Ok(Self { page_table, ipa_bits, mapped_pages: 0 })
    }

    pub fn ipa_bits(&self) -> u8 {
        self.ipa_bits
    }

    pub fn mapped_pages(&self) -> usize {
        self.mapped_pages
    }

    pub fn map_page(&mut self, ipa: usize, pa: usize, attrs: MapAttrs) -> Result<(), Error> {
        self.validate_ipa_page(ipa)?;
        validate_pa_page(pa)?;

        let frame = Frame {
            base: PAddr(pa),
            size: FrameSize::Size4K,
            attr: attrs.into_mem_attr(),
        };
        let Some(allocator) = GLOBAL_ALLOCATOR.get() else {
            return Err(Error::Busy);
        };
        self.page_table
            .map(allocator, VAddr(ipa), frame)
            .map_err(|_| Error::AlreadyMapped)?;
        self.mapped_pages += 1;
        Ok(())
    }

    pub fn unmap_page(&mut self, ipa: usize) -> Result<(), Error> {
        self.validate_ipa_page(ipa)?;
        let Some(allocator) = GLOBAL_ALLOCATOR.get() else {
            return Err(Error::Busy);
        };
        self.page_table
            .unmap(allocator, VAddr(ipa))
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

    /// Destroy an empty page table and return its root frame to the memory pool.
    ///
    /// If mappings remain, ownership of `self` is returned to the caller.
    pub fn destroy_empty(self) -> Result<(), Self> {
        if self.mapped_pages != 0 {
            return Err(self);
        }
        let Some(allocator) = GLOBAL_ALLOCATOR.get() else {
            return Err(self);
        };
        let Self { page_table, .. } = self;
        page_table.drop(allocator);
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

fn validate_mem_pool(
    table_hva_base: usize,
    table_frame_count: usize,
    hva_to_pa_offset: usize,
    ipa_bits: u8,
) -> Result<(), Error> {
    if table_hva_base % PAGE_SIZE != 0
        || hva_to_pa_offset % PAGE_SIZE != 0
        || hva_to_pa_offset > table_hva_base
        || table_frame_count == 0
        || table_frame_count > BIT_ALLOC_CAPACITY
        || !(MIN_IPA_BITS..=MAX_IPA_BITS).contains(&ipa_bits)
    {
        return Err(Error::InvalidArgument);
    }

    let table_bytes = table_frame_count
        .checked_mul(PAGE_SIZE)
        .ok_or(Error::InvalidArgument)?;
    table_hva_base
        .checked_add(table_bytes)
        .ok_or(Error::InvalidArgument)?;
    table_hva_base
        .checked_add(BIT_ALLOC_ADDRESS_SPAN)
        .ok_or(Error::InvalidArgument)?;

    let table_pa_base = table_hva_base - hva_to_pa_offset;
    let table_pa_end = table_pa_base
        .checked_add(table_bytes)
        .ok_or(Error::InvalidArgument)?;
    if table_pa_end == 0 || table_pa_end - 1 > MAX_PA {
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

/// Initialize the shared allocator from Jailhouse's reserved memory pool.
/// The page-table create entry point performs the same initialization lazily,
/// so Jailhouse may call this explicitly during paging setup or omit it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn verihymem_jailhouse_mem_pool_init(
    table_hva_base: usize,
    table_frame_count: usize,
    hva_to_pa_offset: usize,
) -> i32 {
    if validate_mem_pool(
        table_hva_base,
        table_frame_count,
        hva_to_pa_offset,
        MIN_IPA_BITS,
    )
    .is_err()
    {
        return Error::InvalidArgument as i32;
    }
    match unsafe { init_global_allocator(table_hva_base, table_frame_count, hva_to_pa_offset) } {
        Ok(_) => 0,
        Err(err) => err as i32,
    }
}

/// Allocate and initialize one opaque VeriHyMem page-table instance.
///
/// The memory pool remains owned by Jailhouse; this handle only records the
/// allocator/page-table state that operates on it. A null return means that the
/// supplied geometry is outside the first integration envelope.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn verihymem_jailhouse_pt_create(
    table_hva_base: usize,
    table_frame_count: usize,
    hva_to_pa_offset: usize,
    ipa_bits: u8,
) -> *mut JailhousePageTable {
    match unsafe {
        JailhousePageTable::new(
            table_hva_base,
            table_frame_count,
            hva_to_pa_offset,
            ipa_bits,
        )
    } {
        Ok(table) => Box::into_raw(Box::new(table)),
        Err(_) => core::ptr::null_mut(),
    }
}

/// Map one 4 KiB page into an opaque page-table instance.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn verihymem_jailhouse_pt_map_page(
    handle: *mut JailhousePageTable,
    ipa: usize,
    pa: usize,
    attrs: MapAttrs,
) -> i32 {
    let Some(table) = (unsafe { handle.as_mut() }) else {
        return Error::InvalidArgument as i32;
    };
    match table.map_page(ipa, pa, attrs) {
        Ok(()) => 0,
        Err(err) => err as i32,
    }
}

/// Unmap one 4 KiB page from an opaque page-table instance.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn verihymem_jailhouse_pt_unmap_page(
    handle: *mut JailhousePageTable,
    ipa: usize,
) -> i32 {
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
pub unsafe extern "C" fn verihymem_jailhouse_pt_query(
    handle: *const JailhousePageTable,
    ipa: usize,
    out: *mut Mapping,
) -> i32 {
    let (Some(table), Some(out)) = (unsafe { handle.as_ref() }, unsafe { out.as_mut() }) else {
        return Error::InvalidArgument as i32;
    };
    match table.query(ipa) {
        Ok(mapping) => {
            *out = mapping;
            0
        },
        Err(err) => err as i32,
    }
}

/// Destroy an empty page-table instance and release its table frames.
/// Returns `Busy` if mappings remain or the handle is null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn verihymem_jailhouse_pt_destroy(
    handle: *mut JailhousePageTable,
) -> i32 {
    if handle.is_null() {
        return Error::InvalidArgument as i32;
    }
    let table = unsafe { Box::from_raw(handle) };
    match table.destroy_empty() {
        Ok(()) => 0,
        Err(table) => {
            core::mem::forget(table);
            Error::Busy as i32
        },
    }
}

unsafe extern "C" {
    fn verihymem_jailhouse_abort() -> !;
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    unsafe { verihymem_jailhouse_abort() }
}
