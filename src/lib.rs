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
use verified_hv_mem::{
    address::{
        addr::{PAddr, VAddr},
        frame::{Frame, FrameSize, MemAttr},
        region::MemoryRegion,
    },
    bitmap_allocator::bitmap_impl::BitAlloc4K,
    global_allocator::GlobalAllocator,
    hardware::Aarch64Hw,
    hv_mem::{HvMem, protocol::BudgetProtocol},
    memory_set::VecMemorySet,
    page_table::{
        Aarch64PTE, ExPageTable, PTConstants, PageTable,
        pt_arch::{PTArch, PTArchLevel},
    },
};
use vstd::prelude::Tracked;

pub const PAGE_SIZE: usize = 0x1000;
pub const THREE_LEVEL_IPA_BITS: u8 = 39;
pub const FOUR_LEVEL_MIN_IPA_BITS: u8 = 44;
pub const MAX_IPA_BITS: u8 = 48;
pub const MAX_PA: usize = (1usize << MAX_IPA_BITS) - 1;

const BIT_ALLOC_CAPACITY: usize = 1 << 12;
const BIT_ALLOC_ADDRESS_SPAN: usize = BIT_ALLOC_CAPACITY * PAGE_SIZE;

/// Configuration of the dedicated Jailhouse frame pool used by VeriHyMem's global allocator.
#[derive(Clone, Copy)]
struct GlobalFramePoolConfig {
    hva_base: usize,
    frame_count: usize,
    hva_to_pa_offset: usize,
}

/// VeriHyMem's verified global frame allocator specialization.
///
/// This is distinct from the Rust global heap allocator in `heap.rs`.
pub type GlobalFrameAllocator = GlobalAllocator<BitAlloc4K>;

/// The concrete page table type used by Jailhouse.
pub type ConcretePageTable = ExPageTable<BitAlloc4K, Aarch64PTE>;
/// The concrete memory set type used by Jailhouse.
pub type ConcreteMemorySet = VecMemorySet<ConcretePageTable, BitAlloc4K, Aarch64Hw>;
/// Hypervisor memory management object used by Jailhouse.
pub type JailhouseHvMem =
    HvMem<ConcretePageTable, ConcreteMemorySet, BitAlloc4K, BudgetProtocol, Aarch64Hw>;

fn page_table_constants(ipa_bits: u8, hva_to_pa_offset: usize) -> PTConstants {
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
    PTConstants {
        arch: PTArch(levels),
        huge_pages: true,
        hva_to_pa_offset,
    }
}

/// The runtime state for the VeriHyMem integration with Jailhouse.
struct VjRuntime {
    config: GlobalFramePoolConfig,
    hv_mem: JailhouseHvMem,
}

// Jailhouse's bootstrap mapping reaches initialized data before the final
// hypervisor mappings are installed. Keep this early-init singleton there
// instead of allowing zero initialization to place it at the end of .bss.
#[cfg_attr(not(test), unsafe(link_section = ".data"))]
static VJ_RUNTIME: Once<VjRuntime> = Once::new();

/// Error codes for the VeriHyMem integration with Jailhouse.
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
unsafe fn init_runtime(
    frame_pool_hva_base: usize,
    frame_pool_frame_count: usize,
    hva_to_pa_offset: usize,
) -> Result<&'static VjRuntime, Error> {
    validate_global_frame_pool(
        frame_pool_hva_base,
        frame_pool_frame_count,
        hva_to_pa_offset,
        THREE_LEVEL_IPA_BITS,
    )?;

    let runtime = VJ_RUNTIME.call_once(|| {
        let config = GlobalFramePoolConfig {
            hva_base: frame_pool_hva_base,
            frame_count: frame_pool_frame_count,
            hva_to_pa_offset,
        };
        let frame_pool_bytes = config.frame_count * PAGE_SIZE;
        unsafe {
            ptr::write_bytes(config.hva_base as *mut u8, 0, frame_pool_bytes);
        }

        let allocator = GlobalFrameAllocator::default(PAddr(config.hva_base));
        // Trusted handoff from Jailhouse's dedicated frame pool.  After this
        // point the allocator and every page-table client are owned by HvMem.
        allocator.init(config.frame_count, Tracked::assume_new());

        let hv_mem = JailhouseHvMem::new(
            allocator,
            page_table_constants(THREE_LEVEL_IPA_BITS, config.hva_to_pa_offset),
        );
        VjRuntime { config, hv_mem }
    });

    if runtime.config.hva_base != frame_pool_hva_base
        || runtime.config.frame_count != frame_pool_frame_count
        || runtime.config.hva_to_pa_offset != hva_to_pa_offset
    {
        return Err(Error::Busy);
    }
    Ok(runtime)
}

/// C wire representation of VeriHyMem's [`MemAttr`].
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

impl TryFrom<CMapAttrs> for MemAttr {
    type Error = Error;

    fn try_from(attrs: CMapAttrs) -> Result<Self, Self::Error> {
        fn flag(value: u8) -> Result<bool, Error> {
            match value {
                0 => Ok(false),
                1 => Ok(true),
                _ => Err(Error::InvalidArgument),
            }
        }

        Ok(MemAttr::new(
            flag(attrs.readable)?,
            flag(attrs.writable)?,
            flag(attrs.executable)?,
            flag(attrs.device)?,
        ))
    }
}

impl From<MemAttr> for CMapAttrs {
    fn from(attrs: MemAttr) -> Self {
        Self {
            readable: attrs.readable as u8,
            writable: attrs.writable as u8,
            executable: attrs.executable as u8,
            device: attrs.device as u8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct CMapping {
    pub ipa_base: usize,
    pub pa_base: usize,
    pub size: usize,
    pub attrs: CMapAttrs,
}

/// C wire representation of an address translated through an HvMem region set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct CQueryResult {
    pub paddr: usize,
    pub attrs: CMapAttrs,
}

/// One executable VeriHyMem page table backed by the dedicated global frame pool.
pub struct JailhousePageTable {
    page_table: ConcretePageTable,
    ipa_bits: u8,
    mapped_pages: usize,
}

impl JailhousePageTable {
    /// Construct an empty AArch64 stage-2 page table from the shared runtime.
    fn new(runtime: &VjRuntime, ipa_bits: u8) -> Self {
        let constants = page_table_constants(ipa_bits, runtime.config.hva_to_pa_offset);
        let page_table = ConcretePageTable::new(&runtime.hv_mem.allocator, constants);

        Self {
            page_table,
            ipa_bits,
            mapped_pages: 0,
        }
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

    pub fn map(&mut self, ipa: usize, pa: usize, size: usize, attrs: MemAttr) -> Result<(), Error> {
        let frame_size = to_frame_size(size)?;
        self.validate_ipa_mapping(ipa, size)?;
        validate_pa_mapping(pa, size)?;

        let frame = Frame {
            base: PAddr(pa),
            size: frame_size,
            attr: attrs,
        };
        let Some(runtime) = VJ_RUNTIME.get() else {
            return Err(Error::Busy);
        };
        self.page_table
            .map(&runtime.hv_mem.allocator, VAddr(ipa), frame)
            .map_err(|_| Error::AlreadyMapped)?;
        self.mapped_pages += size / PAGE_SIZE;
        Ok(())
    }

    pub fn unmap(&mut self, ipa: usize) -> Result<(), Error> {
        self.validate_ipa_page(ipa)?;
        let Some(runtime) = VJ_RUNTIME.get() else {
            return Err(Error::Busy);
        };
        let frame = self
            .page_table
            .unmap(&runtime.hv_mem.allocator, VAddr(ipa))
            .map_err(|_| Error::NotMapped)?;
        let mapped_pages = frame.size.as_usize() / PAGE_SIZE;
        self.mapped_pages -= mapped_pages;
        Ok(())
    }

    pub fn query(&self, ipa: usize) -> Result<CMapping, Error> {
        self.validate_ipa(ipa)?;
        let (ipa_base, frame) = self
            .page_table
            .query(VAddr(ipa))
            .map_err(|_| Error::NotMapped)?;
        Ok(CMapping {
            ipa_base: ipa_base.0,
            pa_base: frame.base.0,
            size: frame.size.as_usize(),
            attrs: frame.attr.into(),
        })
    }

    /// Destroy an empty page table and return its root frame to the dedicated pool.
    ///
    /// If mappings remain, ownership of `self` is returned to the caller.
    pub fn destroy_empty(self: Box<Self>) -> Result<(), Box<Self>> {
        if self.mapped_pages != 0 {
            return Err(self);
        }
        let Some(runtime) = VJ_RUNTIME.get() else {
            return Err(self);
        };
        let Self { page_table, .. } = *self;
        page_table.drop(&runtime.hv_mem.allocator);
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

    fn validate_ipa_mapping(&self, ipa: usize, size: usize) -> Result<(), Error> {
        self.validate_ipa(ipa)?;
        if ipa % size != 0 || size > (1usize << self.ipa_bits) - ipa {
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
    let allocator_pa_end = frame_pool_pa_base
        .checked_add(BIT_ALLOC_ADDRESS_SPAN)
        .ok_or(Error::InvalidArgument)?;
    if allocator_pa_end == 0 || allocator_pa_end - 1 > MAX_PA {
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

fn to_frame_size(size: usize) -> Result<FrameSize, Error> {
    match size {
        0x1000 => Ok(FrameSize::Size4K),
        0x20_0000 => Ok(FrameSize::Size2M),
        0x4000_0000 => Ok(FrameSize::Size1G),
        _ => Err(Error::InvalidArgument),
    }
}

fn validate_pa_mapping(pa: usize, size: usize) -> Result<(), Error> {
    if pa % size != 0 || pa > MAX_PA - (size - 1) {
        return Err(Error::InvalidArgument);
    }
    Ok(())
}

fn runtime() -> Result<&'static VjRuntime, Error> {
    VJ_RUNTIME.get().ok_or(Error::Busy)
}

fn runtime_for_pool(
    frame_pool_hva_base: usize,
    frame_pool_frame_count: usize,
    hva_to_pa_offset: usize,
    ipa_bits: u8,
) -> Result<&'static VjRuntime, Error> {
    validate_global_frame_pool(
        frame_pool_hva_base,
        frame_pool_frame_count,
        hva_to_pa_offset,
        ipa_bits,
    )?;
    let runtime = runtime()?;
    if runtime.config.hva_base != frame_pool_hva_base
        || runtime.config.frame_count != frame_pool_frame_count
        || runtime.config.hva_to_pa_offset != hva_to_pa_offset
    {
        return Err(Error::Busy);
    }
    Ok(runtime)
}

fn validate_zone_id(zone_id: usize) -> Result<(), Error> {
    if !(1..=u8::MAX as usize).contains(&zone_id) {
        return Err(Error::InvalidArgument);
    }
    Ok(())
}

fn validate_zone_ipa(ipa: usize) -> Result<(), Error> {
    if ipa >= (1usize << THREE_LEVEL_IPA_BITS) {
        return Err(Error::InvalidArgument);
    }
    Ok(())
}

fn zone_region(
    ipa_start: usize,
    pa_start: usize,
    size: usize,
    attrs: MemAttr,
) -> Result<MemoryRegion, Error> {
    validate_zone_ipa(ipa_start)?;
    validate_pa_page(pa_start)?;
    if ipa_start % PAGE_SIZE != 0 || size == 0 || size % PAGE_SIZE != 0 {
        return Err(Error::InvalidArgument);
    }
    let ipa_end = ipa_start.checked_add(size).ok_or(Error::InvalidArgument)?;
    let pa_end = pa_start.checked_add(size).ok_or(Error::InvalidArgument)?;
    if ipa_end > (1usize << THREE_LEVEL_IPA_BITS) || pa_end == 0 || pa_end - 1 > MAX_PA {
        return Err(Error::InvalidArgument);
    }
    Ok(MemoryRegion {
        vstart: VAddr(ipa_start),
        pstart: PAddr(pa_start),
        pages: size / PAGE_SIZE,
        attr: attrs,
    })
}

fn zone_unmap_key(ipa_start: usize) -> Result<MemoryRegion, Error> {
    validate_zone_ipa(ipa_start)?;
    if ipa_start % PAGE_SIZE != 0 {
        return Err(Error::InvalidArgument);
    }
    Ok(MemoryRegion {
        vstart: VAddr(ipa_start),
        pstart: PAddr(0),
        pages: 1,
        attr: MemAttr::new(false, false, false, false),
    })
}

/// Initialize the global frame allocator from its dedicated frame pool.
/// The page-table create entry point performs the same initialization lazily,
/// so Jailhouse may call this explicitly during paging setup or omit it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vj_runtime_init(
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
        init_runtime(
            frame_pool_hva_base,
            frame_pool_frame_count,
            hva_to_pa_offset,
        )
    } {
        Ok(_) => 0,
        Err(err) => err as i32,
    }
}

/// Register one non-root Jailhouse cell as an empty VeriHyMem zone.
#[unsafe(no_mangle)]
pub extern "C" fn vj_hv_add_zone(zone_id: usize) -> i32 {
    if let Err(err) = validate_zone_id(zone_id) {
        return err as i32;
    }
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(err) => return err as i32,
    };
    match runtime.hv_mem.add_zone(zone_id) {
        Ok(()) => 0,
        Err(()) => Error::AlreadyMapped as i32,
    }
}

/// Remove an empty non-root zone.
#[unsafe(no_mangle)]
pub extern "C" fn vj_hv_remove_zone(zone_id: usize) -> i32 {
    if let Err(err) = validate_zone_id(zone_id) {
        return err as i32;
    }
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(err) => return err as i32,
    };
    match runtime.hv_mem.remove_zone(zone_id) {
        Ok(()) => 0,
        Err(()) => Error::Busy as i32,
    }
}

/// Map one contiguous 4 KiB-granular region into a non-root CPU stage-2 table.
#[unsafe(no_mangle)]
pub extern "C" fn vj_hv_map_region(
    zone_id: usize,
    ipa_start: usize,
    pa_start: usize,
    size: usize,
    attrs: CMapAttrs,
) -> i32 {
    if let Err(err) = validate_zone_id(zone_id) {
        return err as i32;
    }
    let attrs = match MemAttr::try_from(attrs) {
        Ok(attrs) => attrs,
        Err(err) => return err as i32,
    };
    let region = match zone_region(ipa_start, pa_start, size, attrs) {
        Ok(region) => region,
        Err(err) => return err as i32,
    };
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(err) => return err as i32,
    };
    match runtime.hv_mem.insert_region(zone_id, region) {
        Ok(()) => 0,
        Err(()) => Error::AlreadyMapped as i32,
    }
}

/// Unmap the CPU region beginning at `ipa_start`.
#[unsafe(no_mangle)]
pub extern "C" fn vj_hv_unmap_region(zone_id: usize, ipa_start: usize) -> i32 {
    if let Err(err) = validate_zone_id(zone_id) {
        return err as i32;
    }
    let region = match zone_unmap_key(ipa_start) {
        Ok(region) => region,
        Err(err) => return err as i32,
    };
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(err) => return err as i32,
    };
    match runtime.hv_mem.remove_region(zone_id, region) {
        Ok(()) => 0,
        Err(()) => Error::NotMapped as i32,
    }
}

/// Translate one CPU stage-2 virtual address in a non-root zone.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vj_hv_query_vaddr(
    zone_id: usize,
    ipa: usize,
    out: *mut CQueryResult,
) -> i32 {
    if let Err(err) = validate_zone_id(zone_id).and_then(|()| validate_zone_ipa(ipa)) {
        return err as i32;
    }
    let Some(out) = (unsafe { out.as_mut() }) else {
        return Error::InvalidArgument as i32;
    };
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(err) => return err as i32,
    };
    match runtime.hv_mem.query_vaddr(zone_id, VAddr(ipa)) {
        Ok((paddr, attrs)) => {
            *out = CQueryResult {
                paddr: paddr.0,
                attrs: attrs.into(),
            };
            0
        }
        Err(()) => Error::NotMapped as i32,
    }
}

/// Return a non-root zone's CPU stage-2 root physical address.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vj_hv_pt_root(zone_id: usize, out_root_pa: *mut usize) -> i32 {
    if let Err(err) = validate_zone_id(zone_id) {
        return err as i32;
    }
    let Some(out_root_pa) = (unsafe { out_root_pa.as_mut() }) else {
        return Error::InvalidArgument as i32;
    };
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(err) => return err as i32,
    };
    match runtime.hv_mem.pt_root(zone_id) {
        Ok(root) => {
            *out_root_pa = root.0;
            0
        }
        Err(()) => Error::NotMapped as i32,
    }
}

/// Map one contiguous region into a non-root IOMMU stage-2 table.
#[unsafe(no_mangle)]
pub extern "C" fn vj_hv_iommu_map_region(
    zone_id: usize,
    ipa_start: usize,
    pa_start: usize,
    size: usize,
    attrs: CMapAttrs,
) -> i32 {
    if let Err(err) = validate_zone_id(zone_id) {
        return err as i32;
    }
    let attrs = match MemAttr::try_from(attrs) {
        Ok(attrs) => attrs,
        Err(err) => return err as i32,
    };
    let region = match zone_region(ipa_start, pa_start, size, attrs) {
        Ok(region) => region,
        Err(err) => return err as i32,
    };
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(err) => return err as i32,
    };
    match runtime.hv_mem.insert_iommu_region(zone_id, region) {
        Ok(()) => 0,
        Err(()) => Error::AlreadyMapped as i32,
    }
}

/// Unmap the IOMMU region beginning at `ipa_start`.
#[unsafe(no_mangle)]
pub extern "C" fn vj_hv_iommu_unmap_region(zone_id: usize, ipa_start: usize) -> i32 {
    if let Err(err) = validate_zone_id(zone_id) {
        return err as i32;
    }
    let region = match zone_unmap_key(ipa_start) {
        Ok(region) => region,
        Err(err) => return err as i32,
    };
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(err) => return err as i32,
    };
    match runtime.hv_mem.remove_iommu_region(zone_id, region) {
        Ok(()) => 0,
        Err(()) => Error::NotMapped as i32,
    }
}

/// Translate one IOMMU stage-2 virtual address in a non-root zone.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vj_hv_iommu_query_vaddr(
    zone_id: usize,
    ipa: usize,
    out: *mut CQueryResult,
) -> i32 {
    if let Err(err) = validate_zone_id(zone_id).and_then(|()| validate_zone_ipa(ipa)) {
        return err as i32;
    }
    let Some(out) = (unsafe { out.as_mut() }) else {
        return Error::InvalidArgument as i32;
    };
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(err) => return err as i32,
    };
    match runtime.hv_mem.iommu_query_vaddr(zone_id, VAddr(ipa)) {
        Ok((paddr, attrs)) => {
            *out = CQueryResult {
                paddr: paddr.0,
                attrs: attrs.into(),
            };
            0
        }
        Err(()) => Error::NotMapped as i32,
    }
}

/// Return a non-root zone's IOMMU stage-2 root physical address.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vj_hv_iommu_pt_root(zone_id: usize, out_root_pa: *mut usize) -> i32 {
    if let Err(err) = validate_zone_id(zone_id) {
        return err as i32;
    }
    let Some(out_root_pa) = (unsafe { out_root_pa.as_mut() }) else {
        return Error::InvalidArgument as i32;
    };
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(err) => return err as i32,
    };
    match runtime.hv_mem.iommu_pt_root(zone_id) {
        Ok(root) => {
            *out_root_pa = root.0;
            0
        }
        Err(()) => Error::NotMapped as i32,
    }
}

/// Allocate and initialize one opaque VeriHyMem page-table instance.
///
/// `out_handle` receives the opaque handle on success and is set to null on
/// failure. `vj_runtime_init` must have initialized the shared
/// frame pool first; this handle records only its page-table client state.
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

    let runtime = match runtime_for_pool(
        frame_pool_hva_base,
        frame_pool_frame_count,
        hva_to_pa_offset,
        ipa_bits,
    ) {
        Ok(runtime) => runtime,
        Err(err) => return err as i32,
    };
    let table = JailhousePageTable::new(runtime, ipa_bits);
    *out_handle = Box::into_raw(Box::new(table));
    0
}

/// Map one aligned 4 KiB page, 2 MiB block, or 1 GiB block.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vj_pt_map(
    handle: *mut JailhousePageTable,
    ipa: usize,
    pa: usize,
    size: usize,
    attrs: CMapAttrs,
) -> i32 {
    let Some(table) = (unsafe { handle.as_mut() }) else {
        return Error::InvalidArgument as i32;
    };
    let Ok(attrs) = MemAttr::try_from(attrs) else {
        return Error::InvalidArgument as i32;
    };
    match table.map(ipa, pa, size, attrs) {
        Ok(()) => 0,
        Err(err) => err as i32,
    }
}

/// Unmap the page or block whose virtual base is `ipa`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vj_pt_unmap(handle: *mut JailhousePageTable, ipa: usize) -> i32 {
    let Some(table) = (unsafe { handle.as_mut() }) else {
        return Error::InvalidArgument as i32;
    };
    match table.unmap(ipa) {
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
            *out = mapping;
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
    fn frame_pool_validation_covers_full_allocator_span() {
        let pa_limit = 1usize << MAX_IPA_BITS;
        assert_eq!(
            validate_global_frame_pool(
                pa_limit - BIT_ALLOC_ADDRESS_SPAN,
                1,
                0,
                THREE_LEVEL_IPA_BITS,
            ),
            Ok(()),
        );
        assert_eq!(
            validate_global_frame_pool(pa_limit - PAGE_SIZE, 1, 0, THREE_LEVEL_IPA_BITS),
            Err(Error::InvalidArgument),
        );
    }

    #[test]
    fn c_abi_page_table_lifecycle_and_busy_destroy() {
        assert!(page_table_constants(THREE_LEVEL_IPA_BITS, 0).huge_pages);
        assert_eq!(
            validate_global_frame_pool(0x1000_0000, 2048, 0, THREE_LEVEL_IPA_BITS),
            Ok(())
        );
        let layout = Layout::from_size_align(BIT_ALLOC_CAPACITY * PAGE_SIZE, PAGE_SIZE).unwrap();
        let frame_pool = unsafe { alloc_zeroed(layout) };
        assert!(!frame_pool.is_null());

        // VJ_RUNTIME retains this dedicated pool for process lifetime.
        // Leaking it here models Jailhouse's static-lifetime frame-pool handoff.
        let frame_pool_hva_base = frame_pool as usize;
        let mut handle = core::ptr::null_mut();
        let mut iommu_handle = core::ptr::null_mut();

        assert_eq!(
            unsafe { vj_runtime_init(frame_pool_hva_base, BIT_ALLOC_CAPACITY, 0) },
            0,
        );

        assert_eq!(
            unsafe {
                vj_pt_create(
                    frame_pool_hva_base,
                    BIT_ALLOC_CAPACITY,
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
                vj_pt_create(
                    frame_pool_hva_base,
                    BIT_ALLOC_CAPACITY,
                    0,
                    THREE_LEVEL_IPA_BITS,
                    &mut iommu_handle,
                )
            },
            0,
        );
        assert!(!iommu_handle.is_null());

        assert_eq!(
            unsafe {
                vj_pt_map(
                    handle,
                    1usize << THREE_LEVEL_IPA_BITS,
                    0x8000,
                    PAGE_SIZE,
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
        assert!(root_pa < frame_pool_hva_base + BIT_ALLOC_CAPACITY * PAGE_SIZE);
        assert_eq!(root_pa % PAGE_SIZE, 0);
        let mut iommu_root_pa = 0;
        assert_eq!(
            unsafe { vj_pt_root_pa(iommu_handle, &mut iommu_root_pa) },
            0
        );
        assert_ne!(root_pa, iommu_root_pa);

        let invalid_attrs = CMapAttrs {
            readable: 2,
            writable: 1,
            executable: 0,
            device: 0,
        };
        assert_eq!(
            unsafe { vj_pt_map(handle, 0x4000, 0x8000, PAGE_SIZE, invalid_attrs) },
            Error::InvalidArgument as i32,
        );

        let attrs = CMapAttrs {
            readable: 1,
            writable: 1,
            executable: 0,
            device: 0,
        };
        assert_eq!(
            unsafe { vj_pt_map(handle, 0x4000, 0x8000, PAGE_SIZE, attrs) },
            0,
        );
        assert_eq!(
            unsafe { vj_pt_map(iommu_handle, 0x4000, 0xc000, PAGE_SIZE, attrs) },
            0,
        );

        let mut mapped_pages = 0;
        assert_eq!(unsafe { vj_pt_mapped_pages(handle, &mut mapped_pages) }, 0,);
        assert_eq!(mapped_pages, 1);

        let mut mapping = MaybeUninit::<CMapping>::uninit();
        assert_eq!(
            unsafe { vj_pt_query(handle, 0x4001, mapping.as_mut_ptr()) },
            0,
        );
        let mapping = unsafe { mapping.assume_init() };
        assert_eq!(mapping.ipa_base, 0x4000);
        assert_eq!(mapping.pa_base, 0x8000);
        assert_eq!(mapping.size, PAGE_SIZE);
        assert_eq!(mapping.attrs.readable, 1);
        assert_eq!(mapping.attrs.writable, 1);
        assert_eq!(mapping.attrs.executable, 0);
        assert_eq!(mapping.attrs.device, 0);

        let huge_page_size = FrameSize::Size2M.as_usize();
        assert_eq!(
            unsafe { vj_pt_map(handle, 0x20_0000, 0x40_0000, huge_page_size, attrs) },
            0,
        );
        let mut huge_mapping = MaybeUninit::<CMapping>::uninit();
        assert_eq!(
            unsafe { vj_pt_query(handle, 0x20_1234, huge_mapping.as_mut_ptr()) },
            0,
        );
        let huge_mapping = unsafe { huge_mapping.assume_init() };
        assert_eq!(huge_mapping.ipa_base, 0x20_0000);
        assert_eq!(huge_mapping.pa_base, 0x40_0000);
        assert_eq!(huge_mapping.size, huge_page_size);
        assert_eq!(unsafe { vj_pt_mapped_pages(handle, &mut mapped_pages) }, 0);
        assert_eq!(mapped_pages, 1 + huge_page_size / PAGE_SIZE);

        let gigantic_page_size = FrameSize::Size1G.as_usize();
        assert_eq!(
            unsafe { vj_pt_map(handle, 0x4000_0000, 0x8000_0000, gigantic_page_size, attrs,) },
            0,
        );
        let mut gigantic_mapping = MaybeUninit::<CMapping>::uninit();
        assert_eq!(
            unsafe { vj_pt_query(handle, 0x4000_1234, gigantic_mapping.as_mut_ptr()) },
            0,
        );
        let gigantic_mapping = unsafe { gigantic_mapping.assume_init() };
        assert_eq!(gigantic_mapping.ipa_base, 0x4000_0000);
        assert_eq!(gigantic_mapping.pa_base, 0x8000_0000);
        assert_eq!(gigantic_mapping.size, gigantic_page_size);
        assert_eq!(unsafe { vj_pt_mapped_pages(handle, &mut mapped_pages) }, 0);
        assert_eq!(
            mapped_pages,
            1 + huge_page_size / PAGE_SIZE + gigantic_page_size / PAGE_SIZE
        );

        // Non-root cells use the self-contained HvMem path. CPU and IOMMU
        // mappings are distinct, and a non-empty zone cannot be destroyed.
        assert_eq!(vj_hv_add_zone(0), Error::InvalidArgument as i32);
        assert_eq!(vj_hv_add_zone(1), 0);
        assert_eq!(vj_hv_add_zone(1), Error::AlreadyMapped as i32);

        let mut zone_root_pa = 0;
        let mut zone_iommu_root_pa = 0;
        assert_eq!(unsafe { vj_hv_pt_root(1, &mut zone_root_pa) }, 0);
        assert_eq!(
            unsafe { vj_hv_iommu_pt_root(1, &mut zone_iommu_root_pa) },
            0,
        );
        assert_ne!(zone_root_pa, zone_iommu_root_pa);

        assert_eq!(
            vj_hv_map_region(1, 0x20_0000, 0x40_0000, huge_page_size, attrs),
            0
        );
        assert_eq!(
            vj_hv_iommu_map_region(1, 0x8000, 0x30_0000, PAGE_SIZE, attrs),
            0,
        );
        let mut zone_query = MaybeUninit::<CQueryResult>::uninit();
        assert_eq!(
            unsafe { vj_hv_query_vaddr(1, 0x20_1001, zone_query.as_mut_ptr()) },
            0,
        );
        let zone_query = unsafe { zone_query.assume_init() };
        assert_eq!(zone_query.paddr, 0x40_1001);
        assert_eq!(zone_query.attrs, attrs);

        let mut zone_iommu_query = MaybeUninit::<CQueryResult>::uninit();
        assert_eq!(
            unsafe { vj_hv_iommu_query_vaddr(1, 0x8000, zone_iommu_query.as_mut_ptr()) },
            0,
        );
        let zone_iommu_query = unsafe { zone_iommu_query.assume_init() };
        assert_eq!(zone_iommu_query.paddr, 0x30_0000);
        assert_eq!(zone_iommu_query.attrs, attrs);
        assert_eq!(vj_hv_remove_zone(1), Error::Busy as i32);
        // CPU unmap executes the EL2 TLBI seam and therefore belongs in the
        // Jailhouse/QEMU integration test, not this EL0 host unit test.

        assert_eq!(unsafe { vj_pt_destroy(handle) }, Error::Busy as i32,);
        let mut mapping_after_busy = MaybeUninit::<CMapping>::uninit();
        assert_eq!(
            unsafe { vj_pt_query(handle, 0x4000, mapping_after_busy.as_mut_ptr()) },
            0,
        );

        // Busy destruction preserves the original handle, so it remains usable.
        assert_eq!(unsafe { vj_pt_unmap(handle, 0x4000_0000) }, 0);
        assert_eq!(unsafe { vj_pt_unmap(handle, 0x20_0000) }, 0);
        assert_eq!(unsafe { vj_pt_unmap(handle, 0x4000) }, 0);
        assert_eq!(unsafe { vj_pt_destroy(handle) }, 0);
        assert_eq!(unsafe { vj_pt_unmap(iommu_handle, 0x4000) }, 0);
        assert_eq!(unsafe { vj_pt_destroy(iommu_handle) }, 0);
    }
}
