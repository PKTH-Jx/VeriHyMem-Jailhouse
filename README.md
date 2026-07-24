# verihymem-jailhouse

`verihymem-jailhouse` is the `no_std` executable adapter between Jailhouse and VeriHyMem. It builds as both an `rlib` and a `staticlib`; Jailhouse will consume the static archive in its final hypervisor link.

The first implementation owns concrete `ExPageTable<BitAlloc1M, Aarch64PTE>` instances backed by one dedicated HVA frame pool supplied by Jailhouse. `GlobalFrameAllocator`, the local name for `GlobalAllocator<BitAlloc1M>`, is initialized once in a `spin::Once`; each page-table handle registers an allocator client and the verified allocator maintains client-disjoint frame ownership. The first initialization receives the frame-pool HVA and Jailhouse's `page_offset`, zeros the pool, and keeps every page-table/PTE/root address in PA.

The Rust heap is independent: `JailhouseHeapAllocator` uses Jailhouse-provided heap hooks only for executable Rust metadata such as `Box` and `Vec`; it is not a client of `GlobalFrameAllocator`.

The prototype deliberately assumes frame allocation is infallible. It also uses
`Tracked::assume_new()` for the initial pool-permission handoff and does not
verify this wrapper with Verus. [PROOF_BOUNDARY.md](PROOF_BOUNDARY.md) states the
Jailhouse assumptions, the conditional properties inherited from VeriHyMem,
and the security properties left for future work.

The library exposes the page-table operations through the opaque C ABI in
`include/vj.h`: global frame allocator initialization,
page-table create/map/unmap/query/root/destroy, and the independent heap and
abort hooks. The next integration step is to link this archive into Jailhouse
and use the handle in shadow mode without switching the active cell page table.
