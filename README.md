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

See [JAILHOUSE_INTEGRATION.md](JAILHOUSE_INTEGRATION.md) for the
dependency inventory, original and integrated builds, QEMU/target run procedure,
expected output, and the staged page-table replacement plan.

## Commands

- `make install-target` installs the freestanding AArch64 Rust target once.
- `make rootfs` creates or finishes configuring the reusable ARM64 rootfs.
- `make jailhouse-clean KDIR=/path/to/kernel/build` invokes the Jailhouse subdirectory Makefile to clean its generated files.
- `make jailhouse-original KDIR=/path/to/kernel/build` builds Jailhouse without VeriHyMem.
- `make verihymem` builds the release wrapper archive for `aarch64-unknown-none-softfloat`.
- `make jailhouse-integrated KDIR=/path/to/kernel/build` links and audits Jailhouse with VeriHyMem.
- `make original-image KDIR=/path/to/kernel/build` rebuilds original Jailhouse, installs it, and atomically refreshes its raw ext4 image.
- `make integrated-image KDIR=/path/to/kernel/build` rebuilds integrated Jailhouse, installs it into the copied rootfs, and atomically refreshes its separate image.
- `make run-original KDIR=/path/to/kernel/build` refreshes the original image before booting it in QEMU.
- `make run-integrated KDIR=/path/to/kernel/build` refreshes the integrated image before booting it in QEMU, preventing an older Jailhouse binary from being run accidentally.
- `make verify` runs full Verus verification of the `verified-hv-mem` dependency; it does not verify this wrapper.
- `make jailhouse-object` builds the selectively linked Rust object consumed by Jailhouse.
- `make jailhouse-check-env KDIR=/path/to/kernel/build` checks the Jailhouse cross-build prerequisites.
- `make test` runs the host-side wrapper lifecycle test and validates `include/vj.h` as C11.
- `make check` runs compilation, verification, and testing.
- `make clean KDIR=/path/to/kernel/build` cleans both Cargo and Jailhouse; without `KDIR`, it cleans Cargo only.
