# verihymem-jailhouse

`verihymem-jailhouse` is the `no_std` executable adapter between Jailhouse and VeriHyMem. It builds as both an `rlib` and a `staticlib`; Jailhouse will consume the static archive in its final hypervisor link.

The first implementation owns concrete `ExPageTable<BitAlloc1M, Aarch64PTE>` instances backed by one HVA memory pool reserved by Jailhouse. `GlobalAllocator<BitAlloc1M>` is initialized once in a `spin::Once`; each page-table handle owns only its page-table state and borrows that global allocator. The first initialization receives the pool HVA and Jailhouse's `page_offset`, zeros the pool, and keeps every page-table/PTE/root address in PA.

Tracked frame permissions are deliberately omitted at this integration boundary. They are proof-only and are supplied with `Tracked::assume_new()`; executable ownership of the pool remains a trusted Jailhouse precondition.

The library exposes the same page-table operations through an opaque C ABI:
`mem_pool_init`, `pt_create`, `pt_map_page`, `pt_unmap_page`, `pt_query`, and
`pt_destroy`. `mem_pool_init` is optional because `pt_create` lazily performs
the same `GlobalAllocator` initialization.
Jailhouse still supplies the heap and abort hooks declared in
`include/verihymem_jailhouse.h`. The next integration step is to link this
archive into Jailhouse and use the handle in shadow mode without switching the
active cell page table.
