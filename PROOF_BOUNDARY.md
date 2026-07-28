# VeriHyMem-Jailhouse Prototype Proof Boundary

This document states the proof boundary for the QEMU GICv3/SMMUv3 integration prototype. The VeriHyMem components retain their machine-checked proofs, but this wrapper and the Jailhouse C code are not verified with Verus. Consequently, the properties below are conditional on the Jailhouse integration assumptions; they are not an end-to-end theorem about the complete Jailhouse binary. SMMUv2/MMU-500 and PVU configurations are rejected by integrated builds.

## Two independent allocators

The integration deliberately has two allocators with different roles:

- `GlobalFrameAllocator` is VeriHyMem's `GlobalAllocator<BitAlloc4K>`. It allocates 4 KiB page-table frames from one dedicated frame pool supplied by Jailhouse. VeriHyMem proves that registered allocator clients own disjoint frame sets. CPU and SMMUv3 IOMMU tables are separate clients; the integrated build reserves the allocator's full capacity of 4096 frames by default.
- `JailhouseHeapAllocator` is Rust's global heap allocator. It obtains storage for executable metadata such as `Box<JailhousePageTable>` and the `Vec` used by `PTArch` through the unverified `vj_heap_alloc` and `vj_heap_dealloc` hooks.

The heap is not a client of `GlobalFrameAllocator`, and heap allocations are not covered by the allocator's client-disjointness proof.

## Jailhouse assumptions

The current prototype assumes all of the following:

1. **Dedicated frame pool.** Jailhouse supplies a page-aligned, writable HVA range with static lifetime that is used only by `GlobalFrameAllocator`. Neither Jailhouse nor the Rust heap allocates from or writes to this range after the handoff.
2. **Valid permission handoff.** The tracked permission introduced by `Tracked::assume_new()` represents exactly that concrete frame pool. This is the trusted bridge from unverified Jailhouse memory into VeriHyMem's proved allocator state.
3. **Infallible frame allocation.** The dedicated pool contains enough free frames for every admitted execution. VeriHyMem intentionally assumes frame allocation succeeds. Exhaustion is outside the prototype guarantee and may abort the hypervisor; no allocation-error rollback property is claimed.
4. **Independent heap contract.** Jailhouse's heap hooks honor the requested size and alignment, return uniquely owned live storage, preserve the dedicated frame pool, and accept the matching deallocation exactly once. Heap exhaustion may also abort.
5. **Fixed HVA/PA direct map.** For every frame-pool address, subtracting `hva_to_pa_offset` yields the PA used in page-table entries and the root register, and adding it back yields the dereferenceable HVA. The conversion does not overflow and matches Jailhouse's `page_offset` mapping.
6. **Valid C calls.** Jailhouse passes live, correctly aligned output pointers and opaque handles returned by `pt_create`; it does not forge, alias, reuse, or access a handle after successful destruction. Calls that mutate one handle are externally serialized.
7. **Admitted mappings.** Jailhouse supplies page-aligned IPAs and PAs in the configured address width and assigns those mapped data frames to the target cell according to Jailhouse's own lifecycle rules. The raw page-table layer does not prove ownership of mapped data frames.
8. **Matching architecture.** The active VJ configuration uses the AArch64
   three-level, 4 KiB stage-2 layout and descriptor interpretation represented
   by `PTArch` and `Aarch64PTE`.
9. **Hardware activation.** Before a generated table is activated, the ARM64
   Jailhouse integration compile-time routes every cell, including the root,
   directly to VeriHyMem, cleans the VJ-owned table pool, applies the required
   barriers, programs VTCR_EL2/VTTBR_EL2, and invalidates the current VMID's
   stage-1/stage-2 translations. Correct CPU and IOMMU behavior remains outside
   the VeriHyMem proof.
10. **Abort semantics.** A Rust panic or violated infallibility assumption ends in `vj_abort`; recovery after an abort is not modeled.

## Conditional properties established

Subject to the assumptions above, the integration reuses these proved VeriHyMem properties:

- Page-table frames allocated to distinct registered clients of `GlobalFrameAllocator` are disjoint. Multiple `JailhousePageTable` values may share the allocator without sharing their owned table frames.
- Each page table remains structurally well formed: table frames are allocated from the dedicated pool, page-table mappings do not overlap, mapped bases are aligned to their frame size, and table mutation preserves the page-table invariants.
- Executable `map`, `unmap`, and `query` operations refine VeriHyMem's abstract page-table transitions for the configured architecture.
- Page-table memory is dereferenced through HVA while table descriptors and the exported root use PA, preserving the fixed-offset PA/HVA separation.
- Destroying an empty handle returns all of that client's remaining page-table frames, including its root, to `GlobalFrameAllocator`. A `Busy` destruction leaves the original C handle live.
- The C boundary rejects null handles/output pointers, non-Boolean attribute bytes, unaligned page addresses, and addresses outside the configured wrapper bounds before invoking the corresponding page-table operation.

## Properties not established by this prototype

The current proof boundary does not establish:

- memory safety or functional correctness of the Rust/C wrapper, the Jailhouse C caller, the heap hooks, compiler, linker, or handwritten assembly;
- availability under frame-pool or heap exhaustion, or transactional rollback after allocation failure;
- ownership or disjointness of the guest data frames passed to `pt_map_page`;
- correct cache, TLB, VMID, VTTBR, or multicore behavior;
- semantic coverage of shared memory, `COMM_REGION`, loadable memory, MMIO, subpage mappings, DMA/IOMMU mappings, or huge pages; or
- full-system isolation against a malicious Jailhouse configuration or a bug in code outside VeriHyMem's verified components.

The prototype replaces the selected CPU and SMMUv3 page-table paths for the
QEMU experiment; it does not claim precise behavioral equivalence with every
Jailhouse backend or end-to-end verification of the integration. Closing the
assumptions above is future work toward full security.
