# Jailhouse Build, Run, and VeriHyMem Integration

This prototype targets ARM64 Jailhouse v0.12 at commit `e57d1eff`. The original
and integrated builds use the same Jailhouse source and kernel build tree. The
only build switch is `VJ_DIR`: without it Jailhouse follows its original path;
with it the build adds `vj.c`, `vj-rust.o`, and the Rust-compatible linker
sections.

## 1. Dependencies

The build has four distinct dependency groups:

- Wrapper build: Rust/Cargo, the freestanding ARM64 Rust target, ARM64 GCC and
  binutils, a native C compiler, GNU Make, and the Rust toolchain's bundled
  LLD.
- Jailhouse module build: a configured and fully built ARM64 Linux tree that
  matches the kernel used to boot the target.
- QEMU runtime: an ARM64 Linux root filesystem and `qemu-system-aarch64`.
- Optional checks and helpers: Verus for `make verify`, and Python Mako for
  Jailhouse's configuration-collection helper.

### 1.1 Install host packages

On Ubuntu 22.04, install the wrapper, kernel, and QEMU dependencies with:

```sh
sudo apt-get update
sudo apt-get install --no-install-recommends \
    build-essential bc bison flex libssl-dev libelf-dev \
    gcc-aarch64-linux-gnu libc6-dev-arm64-cross binutils-aarch64-linux-gnu \
    python3 python3-mako qemu-system-arm \
    cpio debootstrap e2fsprogs kmod rsync wget xz-utils
```

`python3-mako`, `qemu-system-arm`, `cpio`, `debootstrap`, `e2fsprogs`, and
`rsync` are not required merely to link `jailhouse.bin`. Mako enables an
optional Jailhouse helper, QEMU runs the virtual target, and the remaining
tools prepare guest filesystems. If kernel BTF debug information is enabled,
also install `dwarves`; the configuration below disables BTF because
Jailhouse does not need it.

Check the mandatory commands before starting a long kernel build:

```sh
for tool in make cc bc bison flex openssl patch awk \
    aarch64-linux-gnu-gcc aarch64-linux-gnu-ld \
    aarch64-linux-gnu-nm aarch64-linux-gnu-objdump \
    aarch64-linux-gnu-readelf qemu-system-aarch64; do
    command -v "$tool" >/dev/null || echo "missing: $tool"
done
```

No output means all listed commands were found.

### 1.2 Prepare the Rust toolchain

Install Rust with `rustup` if necessary, select the project-compatible
toolchain, and add the freestanding target:

```sh
rustup target add aarch64-unknown-none-softfloat
rustc --version
cargo --version
rustup target list --installed | grep -x aarch64-unknown-none-softfloat
```

The root Makefile discovers LLD inside the active Rust sysroot, so a separate
Ubuntu `lld` or `clang` package is not required. `make verify` additionally
requires the `verus` executable used by VeriHyMem. Install the version pinned
by VeriHyMem's own development environment; ordinary Rust compilation does not
require Verus.

### 1.3 Select a matching Linux kernel

Jailhouse is an external kernel module, so `KDIR` must name a configured and
built ARM64 kernel tree. It must contain `include/config/auto.conf`, and the
kernel/module must match the Linux instance that will load `jailhouse.ko`.

Linux 5.10.0 is the reproducible baseline for this Jailhouse v0.12 snapshot.
Jailhouse's own CI downloads `linux-5.10.tar.xz`, applies an embedded
compatibility patch, and then builds it. Later 5.10 point releases or newer
kernels may require adjusted patch context or Jailhouse driver compatibility
changes.

Set paths for the commands below. `LINUX_SRC` may be a source archive
extraction or a checkout at the exact `v5.10` tag:

```sh
VJ_ROOT=/absolute/path/to/verihymem-jailhouse
LINUX_SRC=/absolute/path/to/linux-5.10
LINUX_BUILD=/absolute/path/to/linux-5.10-build
```

Confirm the source version before patching it:

```sh
make -s -C "$LINUX_SRC" kernelversion
```

The expected output is `5.10.0`.

### 1.4 Apply Jailhouse's Linux 5.10 compatibility patch

The main Jailhouse README describes the general ARM requirements but does not
present this complete patch as a user installation step. The authoritative
patch for this revision is embedded in `jailhouse/ci/gen-kernel-build.sh` and
is applied by Jailhouse CI. It exports kernel symbols used by `jailhouse.ko`,
including the ARM64 hyp-stub vectors and generic virtual-memory mapping
helpers.

First verify that it applies cleanly:

```sh
awk '
/^diff --git a\/arch\/arm\/include\/asm\/virt.h / { emit = 1 }
emit && /^EOF$/ { exit }
emit { print }
' "$VJ_ROOT/jailhouse/ci/gen-kernel-build.sh" |
patch --dry-run -d "$LINUX_SRC" -p1
```

Then apply it once:

```sh
awk '
/^diff --git a\/arch\/arm\/include\/asm\/virt.h / { emit = 1 }
emit && /^EOF$/ { exit }
emit { print }
' "$VJ_ROOT/jailhouse/ci/gen-kernel-build.sh" |
patch -d "$LINUX_SRC" -p1
```

Verify the ARM64 part of the patch:

```sh
grep -n 'EXPORT_SYMBOL_GPL(__hyp_stub_vectors)' \
    "$LINUX_SRC/arch/arm64/kernel/hyp-stub.S"
```

The extracted patch also contains ARM32 and x86 changes. They are inert in an
`ARCH=arm64` build. Do not reapply the patch to an already patched source tree.

### 1.5 Configure Linux for the QEMU ARM64 target

Use an out-of-tree build directory so generated files do not modify the source
tree:

```sh
mkdir -p "$LINUX_BUILD"
make -C "$LINUX_SRC" \
    O="$LINUX_BUILD" \
    ARCH=arm64 \
    CROSS_COMPILE=aarch64-linux-gnu- \
    defconfig
```

Do not copy `ci/kernel-config-amd-seattle` unchanged for the QEMU demo. It is a
CI compile-test configuration with `CONFIG_NR_CPUS=8`, while
`qemu-arm64.cell` assigns a 16-CPU bitmap and the documented QEMU machine uses
`-smp 16`. It also does not describe the complete virtio root-disk setup.

Apply the QEMU and Jailhouse-specific configuration:

```sh
"$LINUX_SRC/scripts/config" --file "$LINUX_BUILD/.config" \
    --enable MODULES \
    --enable MODULE_UNLOAD \
    --enable SMP \
    --enable HOTPLUG_CPU \
    --set-val NR_CPUS 16 \
    --enable ARM_PSCI_FW \
    --enable ARM_GIC \
    --enable ARM_GIC_V3 \
    --enable SERIAL_AMBA_PL011 \
    --enable SERIAL_AMBA_PL011_CONSOLE \
    --enable DEVTMPFS \
    --enable DEVTMPFS_MOUNT \
    --enable FW_LOADER \
    --enable BLK_DEV_INITRD \
    --enable EXT4_FS \
    --enable VIRTIO \
    --enable VIRTIO_MMIO \
    --enable VIRTIO_BLK \
    --enable VIRTIO_NET \
    --enable PCI \
    --enable PCI_HOST_GENERIC \
    --enable KALLSYMS \
    --enable KALLSYMS_ALL \
    --disable KVM \
    --disable DEBUG_INFO_BTF

make -C "$LINUX_SRC" \
    O="$LINUX_BUILD" \
    ARCH=arm64 \
    CROSS_COMPILE=aarch64-linux-gnu- \
    olddefconfig
```

The major settings have these purposes:

- `MODULES` permits loading `jailhouse.ko`; `FW_LOADER` loads
  `/lib/firmware/jailhouse.bin`.
- `SMP`, `NR_CPUS=16`, `HOTPLUG_CPU`, and `ARM_PSCI_FW` let Jailhouse remove
  CPUs from Linux and assign them to cells.
- GICv3 and PL011 match QEMU's interrupt controller and serial console.
- virtio MMIO, block, networking, and ext4 allow the documented QEMU guest to
  find `/dev/vda`, mount its root filesystem, and use its network device.
- generic PCI host support covers QEMU's ECAM layout and Jailhouse virtual PCI
  workflows.
- KVM is disabled so this older Jailhouse path retains the expected ARM64 EL2
  hyp-stub environment. BTF is disabled because it is not needed here.

Check the effective configuration after `olddefconfig` resolves dependencies:

```sh
grep -E 'CONFIG_(MODULES|HOTPLUG_CPU|NR_CPUS|VIRTIO_BLK|VIRTIO_MMIO|SERIAL_AMBA_PL011_CONSOLE|KVM)=' \
    "$LINUX_BUILD/.config"
```

The required options should be `y`, `CONFIG_NR_CPUS` should be `16`, and
`CONFIG_KVM` should not appear.

### 1.6 Build and validate the kernel tree

Build the boot image, kernel modules, and device trees:

```sh
make -C "$LINUX_SRC" \
    O="$LINUX_BUILD" \
    ARCH=arm64 \
    CROSS_COMPILE=aarch64-linux-gnu- \
    -j"$(nproc)" \
    Image modules dtbs
```

Validate the artifacts that matter to Jailhouse:

```sh
test -s "$LINUX_BUILD/arch/arm64/boot/Image"
test -f "$LINUX_BUILD/include/config/auto.conf"
test -f "$LINUX_BUILD/Module.symvers"
make -s -C "$LINUX_SRC" O="$LINUX_BUILD" ARCH=arm64 kernelrelease
```

Use the output directory, not the source directory, as `KDIR`:

```sh
make jailhouse-check-env \
    KDIR="$LINUX_BUILD" \
    CROSS_COMPILE=aarch64-linux-gnu-
```

A full kernel build is preferred over `modules_prepare`. In particular,
`modules_prepare` does not create a usable `Module.symvers` when
`CONFIG_MODVERSIONS=y`. The `Image` that boots the target and the build tree
used for `jailhouse.ko` must come from the same configuration and build.

For physical ARM64 hardware, start from the board's supported kernel and
defconfig instead of this QEMU configuration. The matching-tree requirement,
Jailhouse symbol exports, modules, EL2 boot, PSCI CPU offlining, and reserved
memory requirements still apply, while the UART, interrupt-controller, CPU
count, device-tree, and memory layout must match that board.

### 1.7 Current workspace status

At the time this guide was written, this workspace already had GNU Make 4.3,
the ARM64 GCC/binutils cross tools, Python, and QEMU. The freestanding Rust
target was installed during integration, and the Linux 5.10.0 source was
present at `/home/jingx/os/linux-5.10`. A configured build tree and kernel
`Image` are now present at `/home/jingx/os/linux-5.10-build`. The workspace did
not yet have Verus or Python Mako. The running OrbStack kernel has no matching
module build tree and is not the intended place to load Jailhouse: use the
QEMU setup below or an ARM64 board/VM that exposes EL2 and permits CPU
offlining.

## 2. Build Original Jailhouse First

Prepare the matching ARM64 kernel separately, then run:

```sh
make jailhouse-check-env \
    KDIR=/absolute/path/to/linux-build \
    CROSS_COMPILE=aarch64-linux-gnu-

make jailhouse-original \
    KDIR=/absolute/path/to/linux-build \
    CROSS_COMPILE=aarch64-linux-gnu-
```

This intentionally omits `VJ_DIR`. Important outputs are:

```text
jailhouse/driver/jailhouse.ko
jailhouse/hypervisor/jailhouse.bin
jailhouse/configs/arm64/qemu-arm64.cell
jailhouse/configs/arm64/qemu-arm64-inmate-demo.cell
jailhouse/inmates/demos/arm64/gic-demo.bin
jailhouse/tools/jailhouse
```

The userspace tool is built for the target architecture during a cross build.
Build failures before compiling Jailhouse normally indicate an unsuitable
`KDIR`; `Exec format error` while running `jailhouse/tools/jailhouse` means the
ARM64 target binary was accidentally run on a non-ARM64 build host.

## 3. Build a Minimal ARM64 Root Filesystem

Jailhouse is loaded by Linux; neither `jailhouse.bin` nor the kernel `Image` is
a complete virtual-machine disk. The kernel build produces
`arch/arm64/boot/Image`, while QEMU also needs an ARM64 root filesystem
containing an init system, login tools, `jailhouse.ko`, the Jailhouse command,
and the cell/demo files.

The upstream Jailhouse README assumes that `LinuxInstallation.img` already
exists and recommends the separate `jailhouse-images` project for complete
demo images. The recipe below is a project-local alternative based on standard
Ubuntu `debootstrap` and `mkfs.ext4`; it is not copied from upstream Jailhouse
documentation. It is useful for repeatedly replacing a locally modified
`jailhouse.bin` without rebuilding a larger image framework.

This native recipe assumes an ARM64 build host. On an x86-64 host,
`debootstrap --arch=arm64` also needs `qemu-user-static` and a foreign-arch
bootstrap, or use `jailhouse-images`/Buildroot instead.

### 3.1 Create the root filesystem tree

Choose explicit paths. The commands refuse to reuse an existing rootfs
directory so an earlier image is not accidentally overwritten:

```sh
VJ_ROOT=/absolute/path/to/verihymem-jailhouse
ROOTFS_DIR=/absolute/path/to/jailhouse-rootfs
DISK_IMAGE=/absolute/path/to/LinuxInstallation.img

test ! -e "$ROOTFS_DIR" || {
    echo "refusing to reuse ROOTFS_DIR: $ROOTFS_DIR" >&2
    exit 1
}

sudo mkdir -p "$ROOTFS_DIR"
sudo debootstrap \
    --arch=arm64 \
    --variant=minbase \
    jammy \
    "$ROOTFS_DIR" \
    http://ports.ubuntu.com/ubuntu-ports/
```

Install the minimal runtime utilities. Copying the host resolver configuration
allows `apt` inside the new root to resolve package mirrors:

```sh
sudo cp -L --remove-destination \
    /etc/resolv.conf "$ROOTFS_DIR/etc/resolv.conf"
sudo chroot "$ROOTFS_DIR" apt-get update
sudo chroot "$ROOTFS_DIR" apt-get install -y \
    systemd-sysv udev kmod iproute2
```

`udev` is required even though the kernel's `devtmpfs` creates device nodes.
Systemd uses udev events and tags to activate `.device` units such as
`dev-ttyAMA0.device`; without `systemd-udevd`, a configured serial getty can
time out while waiting for `/dev/ttyAMA0` after the kernel has already
registered that UART.

### 3.2 Configure boot and serial login

Configure the raw filesystem as `/dev/vda`, enable a PL011 serial getty, and
set a test-only root password:

```sh
echo 'root:jailhouse' | sudo chroot "$ROOTFS_DIR" chpasswd
echo 'jailhouse-arm64' | sudo tee "$ROOTFS_DIR/etc/hostname"

sudo tee "$ROOTFS_DIR/etc/hosts" >/dev/null <<'EOF'
127.0.0.1 localhost
127.0.1.1 jailhouse-arm64
EOF

echo '/dev/vda / ext4 defaults 0 1' | \
    sudo tee "$ROOTFS_DIR/etc/fstab"
echo 'ttyAMA0' | sudo tee -a "$ROOTFS_DIR/etc/securetty"

sudo mkdir -p "$ROOTFS_DIR/etc/systemd/system/getty.target.wants"
sudo ln -sf /lib/systemd/system/serial-getty@.service \
    "$ROOTFS_DIR/etc/systemd/system/getty.target.wants/serial-getty@ttyAMA0.service"
```

The password is suitable only for an isolated local QEMU test. Use SSH keys or
another provisioning mechanism for a network-accessible VM.

### 3.3 Install the original Jailhouse artifacts

Run the original build in Section 2 first, then place its outputs in the rootfs:

```sh
sudo install -Dm755 \
    "$VJ_ROOT/jailhouse/tools/jailhouse" \
    "$ROOTFS_DIR/usr/local/sbin/jailhouse"

sudo install -Dm644 \
    "$VJ_ROOT/jailhouse/hypervisor/jailhouse.bin" \
    "$ROOTFS_DIR/lib/firmware/jailhouse.bin"

sudo install -Dm644 \
    "$VJ_ROOT/jailhouse/driver/jailhouse.ko" \
    "$ROOTFS_DIR/root/jailhouse/jailhouse.ko"

sudo install -Dm644 \
    "$VJ_ROOT/jailhouse/configs/arm64/qemu-arm64.cell" \
    "$ROOTFS_DIR/root/jailhouse/qemu-arm64.cell"

sudo install -Dm644 \
    "$VJ_ROOT/jailhouse/configs/arm64/qemu-arm64-inmate-demo.cell" \
    "$ROOTFS_DIR/root/jailhouse/qemu-arm64-inmate-demo.cell"

sudo install -Dm755 \
    "$VJ_ROOT/jailhouse/inmates/demos/arm64/gic-demo.bin" \
    "$ROOTFS_DIR/root/jailhouse/gic-demo.bin"
```

Confirm that the userspace command and kernel module are ARM64 artifacts:

```sh
aarch64-linux-gnu-readelf -h \
    "$ROOTFS_DIR/usr/local/sbin/jailhouse" | grep 'Machine:.*AArch64'
aarch64-linux-gnu-readelf -h \
    "$ROOTFS_DIR/root/jailhouse/jailhouse.ko" | grep 'Machine:.*AArch64'
```

### 3.4 Create the raw ext4 image

This layout deliberately has no partition table. QEMU exposes the entire ext4
filesystem as `/dev/vda`, which differs from the `/dev/vda1` placeholder used
for a partitioned distribution image in the upstream README.

The following commands refuse to replace an existing disk image. Move the old
image aside or select a new filename before regenerating it:

```sh
test ! -e "$DISK_IMAGE" || {
    echo "refusing to overwrite DISK_IMAGE: $DISK_IMAGE" >&2
    exit 1
}

truncate -s 2G "$DISK_IMAGE"
sudo mkfs.ext4 \
    -F \
    -L jailhouse-root \
    -d "$ROOTFS_DIR" \
    "$DISK_IMAGE"
```

`mkfs.ext4 -d` copies the prepared directory into the new filesystem without
loop devices or temporary mounts. The resulting disk uses `format=raw`.

To create a new image after rebuilding Jailhouse, reinstall the changed files
into `ROOTFS_DIR`, choose a new `DISK_IMAGE` name, and repeat this subsection.
There is no need to rerun `debootstrap`.

## 4. Run Original Jailhouse

Boot the raw disk from Section 3 with the matching kernel `Image` produced by
Section 1.6:

```sh
qemu-system-aarch64 \
    -cpu cortex-a57 -smp 16 -m 1G \
    -machine virt,gic-version=3,virtualization=on,its=off \
    -nographic \
    -netdev user,id=net -device virtio-net-device,netdev=net \
    -drive file=LinuxInstallation.img,format=raw,id=disk,if=none \
    -device virtio-blk-device,drive=disk \
    -kernel /path/to/Image \
    -append "root=/dev/vda rw rootwait console=ttyAMA0 mem=768M"
```

The `mem=768M` argument is mandatory for the checked-in `qemu-arm64.cell`.
QEMU RAM spans `0x40000000..0x80000000`; Linux uses only the lower 768 MiB and
leaves the upper region for Jailhouse and inmates. The configured kernel has
virtio MMIO, virtio block, and ext4 built in, so this raw-rootfs path does not
need an initramfs.

If boot reports:

```text
Timed out waiting for device /dev/ttyAMA0.
Dependency failed for Serial Getty on ttyAMA0.
```

first look earlier in the kernel log for lines similar to:

```text
9000000.pl011: ttyAMA0 at MMIO 0x9000000 ... is a PL011 rev1
printk: console [ttyAMA0] enabled
```

If those lines are present and kernel messages appear on the QEMU terminal,
the QEMU PL011 device and kernel driver are working. The likely problem is an
older rootfs created without `udev`. Check the reusable rootfs tree on the
host:

```sh
grep -A2 '^Package: udev$' \
    "$ROOTFS_DIR/var/lib/dpkg/status"
```

An empty result means that `udev` is not installed. Exit QEMU first
(`Ctrl-a x` with `-nographic`), then repair the rootfs tree:

```sh
sudo cp -L --remove-destination \
    /etc/resolv.conf "$ROOTFS_DIR/etc/resolv.conf"
sudo chroot "$ROOTFS_DIR" apt-get update
sudo chroot "$ROOTFS_DIR" apt-get install -y udev
```

Choose a new `DISK_IMAGE` filename and repeat Section 3.4. Merely installing
the package in `ROOTFS_DIR` does not alter an image that was already generated
from that directory. If the PL011 registration lines are absent, return to
Section 1 and verify `CONFIG_SERIAL_AMBA_PL011=y` and
`CONFIG_SERIAL_AMBA_PL011_CONSOLE=y` in the exact kernel build used by QEMU.

Log in on `ttyAMA0` as `root` with the test password `jailhouse`, then run:

```sh
cd /root/jailhouse
insmod jailhouse.ko
jailhouse enable qemu-arm64.cell
jailhouse cell list

jailhouse cell create qemu-arm64-inmate-demo.cell
jailhouse cell load inmate-demo gic-demo.bin
jailhouse cell start inmate-demo

jailhouse cell destroy inmate-demo
jailhouse disable
jailhouse disable
poweroff
```

On successful enable, the serial console contains `Initializing Jailhouse
hypervisor`, per-CPU `OK` lines, page-pool statistics, and no `FAILED` or
`FATAL` line. `cell list` shows the root cell. After starting the inmate, the
GIC demo prints periodic interrupt/timing output. Destroying the inmate and
disabling Jailhouse returns its CPU and memory to Linux.

Common runtime failures are meaningful:

- `No such file or directory` from firmware loading: install
  `jailhouse.bin` under `/lib/firmware` or set the firmware-class search path.
- EL2/virtualization errors: the board booted Linux without EL2, or the outer
  hypervisor did not expose nested ARM virtualization.
- CPU-offline errors: PSCI/hotplug support is missing or a CPU cannot be
  removed from Linux.
- Enable-time memory errors: `mem=768M`, the QEMU RAM size, and the physical
  ranges in `qemu-arm64.cell` do not agree.
- Module version/symbol errors: `jailhouse.ko` was not built against the
  running target kernel.
- Root-mount errors involving `/dev/vda1`: the project-local raw image has no
  partition table, so its root is `/dev/vda`.

## 5. Link and Exercise VeriHyMem

The root Makefile automates rootfs preparation, both Jailhouse builds, wrapper
compilation, integrated linking, artifact installation, image creation, and
QEMU startup. To build and audit only the integrated hypervisor:

```sh
make test
make jailhouse-integrated \
    KDIR=/absolute/path/to/linux-build \
    CROSS_COMPILE=aarch64-linux-gnu-
```

To run the entire integrated workflow, including QEMU:

```sh
make run-integrated \
    KDIR=/absolute/path/to/linux-build \
    CROSS_COMPILE=aarch64-linux-gnu-
```

`run-integrated` builds and audits the integrated hypervisor only when its
inputs are stale, installs changed artifacts in the integrated rootfs, and
atomically regenerates the disk image only when needed before starting QEMU.
This prevents a newly built `jailhouse.bin` from being followed by an
accidental boot of an older image while keeping repeated runs incremental. Use
`Ctrl-a x` to exit QEMU. The equivalent original-Jailhouse workflow is:

```sh
make run-original \
    KDIR=/absolute/path/to/linux-build \
    CROSS_COMPILE=aarch64-linux-gnu-
```

The original workflow follows the same incremental dependency rules: it
rebuilds Jailhouse, reinstalls artifacts, and regenerates the image only when
their inputs are stale. Switching between original and integrated builds
forces one clean rebuild because both modes share Jailhouse's output tree.

The default filesystem outputs are:

```text
fs/jailhouse-rootfs
fs/jailhouse-rootfs-verihymem
fs/LinuxInstallation.img
fs/LinuxInstallation-verihymem.img
```

Before linking VeriHyMem, the Makefile builds and installs original Jailhouse
into `ORIGINAL_ROOTFS_DIR`. It then copies that complete tree with `cp -a
--reflink=auto` to `INTEGRATED_ROOTFS_DIR` and installs the integrated
`jailhouse.bin` and `jailhouse.ko` only in the copy. The original rootfs and
image therefore remain available for A/B testing and recovery.

Image refresh uses a temporary file and an atomic rename, so a failed
`mkfs.ext4` does not replace the last complete image. Each
`original-image`, `integrated-image`, `run-original`, or `run-integrated`
invocation refreshes the selected image when its prerequisites are newer.
Override the output paths when a previous image should be retained as a named
snapshot:

```sh
make integrated-image \
    KDIR=/absolute/path/to/linux-build \
    INTEGRATED_ROOTFS_DIR="$PWD/fs/jailhouse-rootfs-verihymem-2" \
    INTEGRATED_DISK_IMAGE="$PWD/fs/LinuxInstallation-verihymem-2.img"
```

The root Makefile performs these stages:

1. Prepare an ARM64 `debootstrap` rootfs with systemd, udev, and serial login.
2. Build original Jailhouse and install its artifacts in the original rootfs.
3. Copy the original rootfs for the integrated build.
4. Build `libverihymem_jailhouse.a` for the freestanding ARM64 Rust target.
5. Relink only the exported C ABI roots into `target/jailhouse/vj-rust.o`.
6. Pass `VJ_DIR` to Jailhouse, which compiles the C heap/abort hooks and links
   the Rust object into `hypervisor.o` before producing `jailhouse.bin`.
7. Reject missing ABI symbols, unresolved symbols, unwinding support, and
   linker sections that would otherwise fall beyond `__page_pool`.
8. Install the integrated artifacts in the copied rootfs and create its image.

The Jailhouse build and cleanup targets delegate to Jailhouse's own Makefile:

```sh
make jailhouse-original KDIR=/absolute/path/to/linux-build
make jailhouse-integrated KDIR=/absolute/path/to/linux-build
make jailhouse-clean KDIR=/absolute/path/to/linux-build
```

The first two targets clean Jailhouse before compiling, which is necessary
when switching between original and integrated compiler flags. `make clean
KDIR=/absolute/path/to/linux-build` cleans both Cargo outputs and Jailhouse;
without `KDIR`, the root `clean` target cleans Cargo outputs only.

## 6. Page-Table Integration

The replacement was developed in two checkpoints before enabling VeriHyMem for
the root cell as well.

### Shadow checkpoint

1. During `paging_init`, reserve a fixed, page-aligned run from `mem_pool` for
   VeriHyMem and never return it to Jailhouse. Pass its HVA, frame count, and
   Jailhouse `page_offset` to `vj_global_frame_allocator_init`. The allocator
   base is deliberately the HVA; `PageTableMem` subtracts `page_offset` when
   storing table/root physical addresses.
2. Add opaque CPU and IOMMU VeriHyMem handles to ARM's `struct arch_cell`.
   Integrated cells, including the root, do not allocate
   `cell->arch.mm.root_table`.
3. Route supported 4 KiB CPU regions from `arch_map_memory_region` and
   `arch_unmap_memory_region` into the CPU handle. Route DMA regions through
   the separate IOMMU handle. Expand regions page by page and translate
   Jailhouse flags into `vj_map_attrs`.
4. The shadow checkpoint compared `vj_pt_query` with `paging_virt2phys` for
   every mapped page and completed a create/load/start/destroy QEMU lifecycle.
   That comparison was test-only and is no longer part of production cell
   creation. Subpage MMIO remains intentionally absent from the cell table.
5. On cell destroy, unmap all mirrored pages, require the mapped-page count to
   reach zero, then call `vj_pt_destroy`.

### Activation checkpoint

ARM64 uses the VJ CPU table when initializing every cell vCPU, including a root
cell CPU. The handoff is compile-time selected: an integrated vCPU calls
`vj_paging_vcpu_init` directly instead of passing `cell->arch.mm` through
Jailhouse's native VTTBR routine. The VJ routine cleans the dedicated table
pool, executes the required DSB/ISB barriers, programs VTCR_EL2 for the table's
IPA width, programs VTTBR_EL2 with the cell VMID and cached root physical
address, and performs a VMID-scoped stage-1/stage-2 TLB invalidation. All
integrated cell create/map/unmap/query/destroy operations are compile-time
routed exclusively to VJ, and the native root and geometry pointers remain
null. Jailhouse's EL2, temporary-mapping, and CPU-parking tables continue to
use the original paging implementation. Initial root construction remains
inactive until all root regions are populated; each root CPU installs the
finalized VJ table at the final VMM activation boundary.

Cells, including the root cell, with assigned stream IDs on an SMMUv3 system
receive a second VJ client containing only DMA regions. SMMUv3 stream-table
entries use that client's 39-bit VTCR geometry and root PA; they never reuse
the CPU client. SMMUv3 is the sole VeriHyMem IOMMU backend; integrated builds
reject SMMUv2/MMU-500 and PVU configurations. The experimental runtime target
is QEMU `virt` with GICv3 and SMMUv3. Table mutations clean the shared VJ frame
pool before an IOMMU can walk it. Configuration commits invalidate both the
root VMID, whose DMA ownership changes during cell transitions, and the
affected non-root VMID.

The current wrapper supports only 4 KiB mappings. It can construct a 39-bit
three-level table or a 44--48-bit four-level table; Jailhouse currently creates
the three-level form for each CPU and IOMMU client. The default dedicated pool
uses all 4096 pages (16 MiB) supported by `BitAlloc4K`. The QEMU system
configuration reserves 32 MiB for the integrated hypervisor so that the frame
pool does not consume the complete Jailhouse reservation. The integrated ARM64
bootstrap maps that complete 32 MiB reservation before C initialization, so
VeriHyMem can safely zero and initialize the pool before the permanent EL2
table is installed.
Jailhouse's huge pages, subpage regions, executable/XN behavior, physical
SMMU implementations, and PVU remain outside the VJ replacement scope. The
trusted assumptions are recorded in
`PROOF_BOUNDARY.md`.
