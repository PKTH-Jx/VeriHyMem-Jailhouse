# VeriHyMem-Jailhouse Prototype Proof Boundary

This document defines the boundary of the ARM64 GICv3/SMMUv3 integration. It is a conditional boundary for the verified VeriHyMem components, not an end-to-end theorem about the Rust/C wrapper or the Jailhouse binary. Integrated builds reject SMMUv2/MMU-500 and PVU configurations.

## Integrated objects

The runtime has two different cell paths:

- The root cell owns standalone `JailhousePageTable` objects. Its CPU and, when needed, IOMMU tables use the `vj_pt_*` ABI.
- Every non-root cell is one `HvMem` zone. A zone owns separate CPU and IOMMU `MemorySet`s, their page-table roots, the per-zone ownership state, and the CPU/IOMMU MMU synchronization tokens. The `vj_hv_*` ABI exposes this path.
- Jailhouse's EL2, temporary-mapping, and CPU-parking tables remain native.

`MemorySet` is the verified region-plus-page-table layer: a region operation updates the region set and expands its dense 4 KiB mappings in the backing page table. `HvMem` maintains the zone registry and shared allocator. The outer `HvMem` lock protects the registry and zone lifetime; the inner `Zone` lock protects one zone's CPU and IOMMU state. Zone reads may run concurrently, and different zones may be mutated concurrently; zone creation/removal is serialized by the outer write lock. The global frame allocator has its own mutex for frame allocation.

## Trusted assumptions

The integration assumes:

1. **Dedicated frame pool.** Jailhouse supplies a page-aligned, writable HVA range with static lifetime. After `vj_runtime_init`, only VeriHyMem's `GlobalAllocator<BitAlloc4K>` uses it.
2. **Permission handoff and capacity.** `Tracked::assume_new()` describes exactly that pool, and the pool never exhausts. Exhaustion may abort; no allocation-failure rollback property is claimed.
3. **Heap contract.** Jailhouse's `vj_heap_alloc`/`vj_heap_dealloc` hooks honor size, alignment, ownership, and matching deallocation. The Rust heap is independent of the page-table frame allocator.
4. **Address conversion.** Subtracting `hva_to_pa_offset` converts every pool HVA to the PA used in descriptors and exported roots, and the reverse conversion yields a dereferenceable HVA.
5. **Valid callers and lifecycle.** Jailhouse uses valid IDs in `1..=255` for non-root zones, valid aligned C arguments, and live handles. It maps and unmaps a non-root region before calling `vj_hv_remove_zone`; it externally serializes mutations of a root `vj_pt_*` handle. The wrapper and Jailhouse C code are not verified.
6. **Admitted non-root regions.** For a zone `zid`, each CPU region belongs to `zone_regions(zid)` and each IOMMU region belongs to that set or the configured GIC region. The `BudgetProtocol` axioms supply validity and physical disjointness of configured regions, including cross-zone disjointness. These are assumptions at the unverified FFI boundary, not runtime ownership checks.
7. **Architecture and activation.** The active configuration uses the AArch64 three-level, 39-bit, 4 KiB table represented by `PTArch` and `Aarch64PTE`. Jailhouse cleans the shared table pool and performs the required CPU and SMMUv3 activation/invalidation sequence before reuse. The AArch64 instruction bodies and handwritten assembly are outside the VeriHyMem proof.

## Conditional properties

Subject to those assumptions, the integrated VeriHyMem code provides:

- structurally well-formed page tables and disjoint ownership of page-table frames allocated to distinct allocator clients;
- exact correspondence between each admitted zone region and its page mappings, with no overlapping virtual regions within a memory set;
- CPU and IOMMU translation confinement to the admitted physical regions, and cross-zone physical disjointness inherited from `BudgetProtocol`;
- a unique live zone for each registered non-root cell ID, protection against zone removal while an `HvMem` read operation holds the outer lock, and serialized mutation of one zone's state under its inner lock;
- synchronization-token refinement of CPU map/unmap maintenance and separate CPU/IOMMU page-table state; and
- checked C-boundary arguments for the exported operations.

The root cell receives only the standalone page-table and allocator properties; it is deliberately outside the `HvMem` zone-level isolation properties.

## Not established

This prototype does not establish:

- memory safety or functional correctness of the wrapper, Jailhouse C code, allocator hooks, compiler, linker, or assembly;
- that Jailhouse's physical cell assignment satisfies the admitted-region premises, or that guest data frames are owned exclusively by their target cell;
- correct cache, TLB, VMID, VTTBR, SMMU command-queue, or multicore hardware behavior;
- transactional recovery after frame-pool or heap exhaustion;
- semantics for huge pages, subpage MMIO, shared/communication regions, executable/XN behavior, or non-SMMUv3 IOMMU backends; or
- full-system isolation against a malformed configuration or a bug outside the verified VeriHyMem modules.

The integration therefore replaces selected Jailhouse CPU and SMMUv3 translation backends. It does not replace Jailhouse's cell lifecycle, configuration validation, physical-memory transfer, or device-management framework.
