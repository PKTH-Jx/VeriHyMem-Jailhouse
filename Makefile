SHELL := /bin/sh

CARGO ?= cargo
RUSTC ?= rustc
RUSTUP ?= rustup
VERUS ?= verus
VERUSFLAGS ?= --multiple-errors 20 --triggers-mode silent
HOST_CC ?= cc
NM ?= nm
GREP ?= grep
AWK ?= awk
WC ?= wc
SUDO ?= sudo
DEBOOTSTRAP ?= debootstrap
MKFS_EXT4 ?= mkfs.ext4
QEMU ?= qemu-system-aarch64

RUST_TARGET ?= aarch64-unknown-none-softfloat
RUSTFLAGS_FREESTANDING ?= -C relocation-model=static -Awarnings
TEST_RUSTFLAGS ?= -Awarnings
TEST_STACK_SIZE ?= 16777216
RUST_HOST := $(shell $(RUSTC) -vV | $(AWK) '/^host:/ { print $$2 }')
RUST_LLD := $(shell $(RUSTC) --print sysroot)/lib/rustlib/$(RUST_HOST)/bin/gcc-ld/ld.lld
LD_LLD ?= $(RUST_LLD)

TARGET_DIR ?= target
ARCHIVE := $(TARGET_DIR)/$(RUST_TARGET)/release/libverihymem_jailhouse.a
CARGO_BUILD_STAMP := $(TARGET_DIR)/$(RUST_TARGET)/release/.verihymem-build
JAILHOUSE_OBJ_DIR := $(TARGET_DIR)/jailhouse
JAILHOUSE_RUST_OBJ := $(JAILHOUSE_OBJ_DIR)/vj-rust.o
JAILHOUSE_DIR ?= jailhouse
JAILHOUSE_MAKE ?= make
JAILHOUSE_ARCH ?= arm64
CROSS_COMPILE ?= aarch64-linux-gnu-
JAILHOUSE_NM ?= $(CROSS_COMPILE)nm
JAILHOUSE_OBJDUMP ?= $(CROSS_COMPILE)objdump
JAILHOUSE_READELF ?= $(CROSS_COMPILE)readelf
KDIR ?=
JAILHOUSE_HYPERVISOR_OBJ := $(JAILHOUSE_DIR)/hypervisor/hypervisor.o
JAILHOUSE_BIN := $(JAILHOUSE_DIR)/hypervisor/jailhouse.bin
JAILHOUSE_TOOL := $(JAILHOUSE_DIR)/tools/jailhouse
JAILHOUSE_KO := $(JAILHOUSE_DIR)/driver/jailhouse.ko
JAILHOUSE_SYSTEM_CELL := $(JAILHOUSE_DIR)/configs/arm64/qemu-arm64.cell
JAILHOUSE_INMATE_CELL := $(JAILHOUSE_DIR)/configs/arm64/qemu-arm64-inmate-demo.cell
JAILHOUSE_INMATE_BIN := $(JAILHOUSE_DIR)/inmates/demos/arm64/gic-demo.bin

FS_DIR ?= $(CURDIR)/fs
ORIGINAL_ROOTFS_DIR ?= $(FS_DIR)/jailhouse-rootfs
INTEGRATED_ROOTFS_DIR ?= $(FS_DIR)/jailhouse-rootfs-verihymem
ORIGINAL_DISK_IMAGE ?= $(FS_DIR)/LinuxInstallation.img
INTEGRATED_DISK_IMAGE ?= $(FS_DIR)/LinuxInstallation-verihymem.img
ROOTFS_STAMP := $(ORIGINAL_ROOTFS_DIR)/.verihymem-jailhouse-rootfs
ORIGINAL_ARTIFACTS_STAMP := $(ORIGINAL_ROOTFS_DIR)/.jailhouse-original-artifacts
INTEGRATED_ROOTFS_STAMP := $(INTEGRATED_ROOTFS_DIR)/.jailhouse-verihymem-artifacts
INTEGRATED_ROOTFS_COPY_STAMP := $(INTEGRATED_ROOTFS_DIR)/.jailhouse-verihymem-rootfs
ORIGINAL_BUILD_STAMP := $(JAILHOUSE_OBJ_DIR)/.jailhouse-original
JAILHOUSE_BUILD_STAMP := $(JAILHOUSE_OBJ_DIR)/.jailhouse-integrated
ORIGINAL_MODE_STAMP := $(JAILHOUSE_OBJ_DIR)/.jailhouse-original-mode
INTEGRATED_MODE_STAMP := $(JAILHOUSE_OBJ_DIR)/.jailhouse-integrated-mode

# Keep the artifact targets sensitive to source changes while allowing normal
# repeated invocations to use Cargo and Jailhouse's own incremental outputs.
VJ_SOURCE_FILES := $(shell git ls-files -- Cargo.toml Cargo.lock Makefile src include) \
	$(addprefix verified-hv-mem/,$(shell git -C verified-hv-mem ls-files -- Cargo.toml Cargo.lock src))
JAILHOUSE_SOURCE_FILES := $(addprefix $(JAILHOUSE_DIR)/,$(shell git -C $(JAILHOUSE_DIR) ls-files))
ROOTFS_SUITE ?= jammy
ROOTFS_MIRROR ?= http://ports.ubuntu.com/ubuntu-ports/
ROOTFS_PACKAGES ?= systemd-sysv udev kmod iproute2
ROOTFS_HOSTNAME ?= jailhouse-arm64
ROOT_PASSWORD ?= jailhouse
DISK_SIZE ?= 2G
DEST_ROOTFS ?=

KERNEL_IMAGE ?= $(KDIR)/arch/arm64/boot/Image
QEMU_CPU ?= cortex-a57
QEMU_CPUS ?= 16
QEMU_RAM ?= 1G
LINUX_RAM ?= 768M
QEMU_APPEND ?= root=/dev/vda rw rootwait console=ttyAMA0 mem=$(LINUX_RAM)
QEMU_EXTRA_ARGS ?=
VJ_FRAME_POOL_PAGES ?= 4096

VJ_ENTRY_POINTS := \
	vj_runtime_init \
	vj_hv_add_zone \
	vj_hv_remove_zone \
	vj_hv_map_region \
	vj_hv_unmap_region \
	vj_hv_query_vaddr \
	vj_hv_pt_root \
	vj_hv_iommu_map_region \
	vj_hv_iommu_unmap_region \
	vj_hv_iommu_query_vaddr \
	vj_hv_iommu_pt_root \
	vj_pt_create \
	vj_pt_map \
	vj_pt_unmap \
	vj_pt_query \
	vj_pt_root_pa \
	vj_pt_mapped_pages \
	vj_pt_destroy
VJ_LINKED_SYMBOLS := $(VJ_ENTRY_POINTS) vj_heap_alloc vj_heap_dealloc vj_abort

.DEFAULT_GOAL := help

.PHONY: help all check install-target compile verify verihymem jailhouse-object \
	jailhouse-check-env jailhouse-clean jailhouse-original jailhouse-build jailhouse-audit \
	jailhouse-link jailhouse-integrated test \
	rootfs-check-env rootfs install-jailhouse-artifacts refresh-original-rootfs original-rootfs \
	integrated-rootfs original-image integrated-image qemu-check \
	run-original run-integrated clean

help:
	@echo "Common targets:"
	@echo "  make rootfs             Build/configure the reusable original ARM64 rootfs"
	@echo "  make jailhouse-clean    Clean Jailhouse through its nested Makefile (requires KDIR)"
	@echo "  make jailhouse-original Build original ARM64 Jailhouse (requires KDIR)"
	@echo "  make verihymem          Build the freestanding VeriHyMem wrapper archive"
	@echo "  make jailhouse-integrated Build, link, and audit Jailhouse + VeriHyMem"
	@echo "  make original-image     Build and refresh the original image when stale"
	@echo "  make integrated-image   Build and refresh the integrated image when stale"
	@echo "  make run-original       Refresh when stale, then launch the original image"
	@echo "  make run-integrated     Refresh when stale, then launch the integrated image"
	@echo "  make verify             Verify verified-hv-mem with Verus"
	@echo "  make test               Run Rust unit tests and validate the C header"
	@echo "  make check              Run compile, verify, and test"
	@echo "  make install-target     Install $(RUST_TARGET) with rustup"
	@echo "  make clean              Clean Cargo; also clean Jailhouse when KDIR is set"
	@echo
	@echo "Required for Jailhouse/QEMU targets: KDIR=/path/to/ARM64/kernel/build"

all: compile

check: compile verify test

install-target:
	$(RUSTUP) target add $(RUST_TARGET)

$(CARGO_BUILD_STAMP): $(VJ_SOURCE_FILES)
	@$(RUSTUP) target list --installed | $(GREP) -qx '$(RUST_TARGET)' || \
		{ echo "missing Rust target $(RUST_TARGET); run 'make install-target'" >&2; exit 1; }
	RUSTFLAGS="$(RUSTFLAGS_FREESTANDING)" \
		$(CARGO) build --locked --release --target $(RUST_TARGET)
	@test -f $(ARCHIVE)
	@touch "$@"

compile: $(CARGO_BUILD_STAMP)

verihymem: compile

verify:
	cd verified-hv-mem && $(VERUS) src/lib.rs $(VERUSFLAGS)

rootfs-check-env:
	@test "$$(uname -m)" = aarch64 || \
		{ echo "the rootfs target currently requires a native AArch64 host" >&2; exit 1; }
	@for tool in "$(SUDO)" "$(DEBOOTSTRAP)" "$(MKFS_EXT4)"; do \
		command -v "$$tool" >/dev/null 2>&1 || \
			{ echo "required rootfs tool not found: $$tool" >&2; exit 1; }; \
	done

$(ROOTFS_STAMP): | rootfs-check-env
	@mkdir -p "$(FS_DIR)"
	@if test -f "$(ORIGINAL_ROOTFS_DIR)/var/lib/dpkg/status"; then \
		echo "using existing ARM64 rootfs tree: $(ORIGINAL_ROOTFS_DIR)"; \
	else \
		test ! -e "$(ORIGINAL_ROOTFS_DIR)" || { \
			echo "refusing non-rootfs path: $(ORIGINAL_ROOTFS_DIR)" >&2; \
			exit 1; \
		}; \
		$(SUDO) mkdir -p "$(ORIGINAL_ROOTFS_DIR)"; \
		$(SUDO) $(DEBOOTSTRAP) --arch=arm64 --variant=minbase \
			"$(ROOTFS_SUITE)" "$(ORIGINAL_ROOTFS_DIR)" "$(ROOTFS_MIRROR)"; \
	fi
	$(SUDO) cp -L --remove-destination \
		/etc/resolv.conf "$(ORIGINAL_ROOTFS_DIR)/etc/resolv.conf"
	$(SUDO) chroot "$(ORIGINAL_ROOTFS_DIR)" apt-get update
	$(SUDO) env DEBIAN_FRONTEND=noninteractive \
		chroot "$(ORIGINAL_ROOTFS_DIR)" apt-get install -y $(ROOTFS_PACKAGES)
	@printf '%s\n' 'root:$(ROOT_PASSWORD)' | \
		$(SUDO) chroot "$(ORIGINAL_ROOTFS_DIR)" chpasswd
	@printf '%s\n' '$(ROOTFS_HOSTNAME)' | \
		$(SUDO) tee "$(ORIGINAL_ROOTFS_DIR)/etc/hostname" >/dev/null
	@printf '%s\n' '127.0.0.1 localhost' '127.0.1.1 $(ROOTFS_HOSTNAME)' | \
		$(SUDO) tee "$(ORIGINAL_ROOTFS_DIR)/etc/hosts" >/dev/null
	@printf '%s\n' '/dev/vda / ext4 defaults 0 1' | \
		$(SUDO) tee "$(ORIGINAL_ROOTFS_DIR)/etc/fstab" >/dev/null
	@$(SUDO) grep -qx 'ttyAMA0' "$(ORIGINAL_ROOTFS_DIR)/etc/securetty" 2>/dev/null || \
		printf '%s\n' 'ttyAMA0' | \
		$(SUDO) tee -a "$(ORIGINAL_ROOTFS_DIR)/etc/securetty" >/dev/null
	$(SUDO) mkdir -p \
		"$(ORIGINAL_ROOTFS_DIR)/etc/systemd/system/getty.target.wants"
	$(SUDO) ln -sf /lib/systemd/system/serial-getty@.service \
		"$(ORIGINAL_ROOTFS_DIR)/etc/systemd/system/getty.target.wants/serial-getty@ttyAMA0.service"
	$(SUDO) touch "$@"
	@echo "prepared original rootfs: $(ORIGINAL_ROOTFS_DIR)"

rootfs: $(ROOTFS_STAMP)

$(JAILHOUSE_OBJ_DIR):
	mkdir -p $@

$(JAILHOUSE_RUST_OBJ): $(CARGO_BUILD_STAMP) | $(JAILHOUSE_OBJ_DIR)
	$(LD_LLD) -r --gc-sections \
		$(foreach symbol,$(VJ_ENTRY_POINTS),--undefined=$(symbol)) \
		-o $@ $(ARCHIVE)

jailhouse-object: $(JAILHOUSE_RUST_OBJ)
	@for symbol in $(VJ_ENTRY_POINTS); do \
		if ! $(NM) -g --defined-only $(JAILHOUSE_RUST_OBJ) | \
			$(GREP) -Eq "[[:space:]][Tt][[:space:]]$$symbol$$"; then \
			echo "missing Jailhouse ABI symbol $$symbol in $(JAILHOUSE_RUST_OBJ)" >&2; \
			exit 1; \
		fi; \
	done
	@unexpected="$$( \
		$(NM) -u $(JAILHOUSE_RUST_OBJ) | $(AWK) '{ print $$NF }' | \
		$(GREP) -Ev '^(memcpy|memset|vj_abort|vj_heap_alloc|vj_heap_dealloc)$$' || true \
	)"; \
	if [ -n "$$unexpected" ]; then \
		echo "unexpected unresolved symbols in $(JAILHOUSE_RUST_OBJ):" >&2; \
		echo "$$unexpected" >&2; \
		exit 1; \
	fi
	@if $(NM) $(JAILHOUSE_RUST_OBJ) | \
		$(GREP) -Eq 'rust_eh_personality|_Unwind_'; then \
		echo "unexpected unwinding support in $(JAILHOUSE_RUST_OBJ)" >&2; \
		exit 1; \
	fi
	@$(JAILHOUSE_READELF) -h $(JAILHOUSE_RUST_OBJ) | \
		$(GREP) -q 'Machine:.*AArch64'
	@echo "built $(JAILHOUSE_RUST_OBJ) (AArch64 relocatable object)"

jailhouse-check-env:
	@test -n "$(KDIR)" || \
		{ echo "KDIR is required; pass the ARM64 Linux kernel build directory" >&2; exit 1; }
	@test -d "$(KDIR)" || \
		{ echo "KDIR does not exist: $(KDIR)" >&2; exit 1; }
	@test -f "$(KDIR)/include/config/auto.conf" || \
		{ echo "KDIR is not a configured kernel build tree (missing include/config/auto.conf): $(KDIR)" >&2; exit 1; }
	@command -v "$(firstword $(JAILHOUSE_MAKE))" >/dev/null 2>&1 || \
		{ echo "Jailhouse make command not found: $(firstword $(JAILHOUSE_MAKE))" >&2; exit 1; }
	@$(JAILHOUSE_MAKE) --version | $(AWK) 'NR == 1 { \
		if ($$1 != "GNU" || $$2 != "Make") exit 1; \
		split($$3, version, "."); \
		exit !(version[1] > 3 || (version[1] == 3 && version[2] >= 82)); \
	}' || { echo "Jailhouse requires GNU Make 3.82 or newer; set JAILHOUSE_MAKE" >&2; exit 1; }
	@for tool in "$(CROSS_COMPILE)gcc" "$(JAILHOUSE_NM)" \
		"$(JAILHOUSE_OBJDUMP)" "$(JAILHOUSE_READELF)"; do \
		command -v "$$tool" >/dev/null 2>&1 || \
			{ echo "ARM64 cross-tool not found: $$tool" >&2; exit 1; }; \
	done

jailhouse-clean: jailhouse-check-env
	$(JAILHOUSE_MAKE) -C $(JAILHOUSE_DIR) clean \
		ARCH=$(JAILHOUSE_ARCH) \
		CROSS_COMPILE=$(CROSS_COMPILE) \
		KDIR=$(abspath $(KDIR)) \
		VJ_DIR=
	rm -f "$(ORIGINAL_MODE_STAMP)" "$(INTEGRATED_MODE_STAMP)" \
		"$(ORIGINAL_BUILD_STAMP)" "$(JAILHOUSE_BUILD_STAMP)"

# Build stamps retain source validity across mode switches. The mutually
# exclusive mode stamps describe which build currently occupies jailhouse/.
$(ORIGINAL_MODE_STAMP): $(ORIGINAL_BUILD_STAMP) | $(JAILHOUSE_OBJ_DIR)
	@if test ! -f "$@"; then \
		$(MAKE) -B "$(ORIGINAL_BUILD_STAMP)"; \
	fi

$(INTEGRATED_MODE_STAMP): $(JAILHOUSE_BUILD_STAMP) | $(JAILHOUSE_OBJ_DIR)
	@if test ! -f "$@"; then \
		$(MAKE) -B "$(JAILHOUSE_BUILD_STAMP)"; \
	fi

$(ORIGINAL_BUILD_STAMP): $(JAILHOUSE_SOURCE_FILES) Makefile | jailhouse-check-env
	+$(JAILHOUSE_MAKE) -C $(JAILHOUSE_DIR) clean \
		ARCH=$(JAILHOUSE_ARCH) \
		CROSS_COMPILE=$(CROSS_COMPILE) \
		KDIR=$(abspath $(KDIR)) \
		VJ_DIR=
	$(JAILHOUSE_MAKE) -C $(JAILHOUSE_DIR) modules \
		ARCH=$(JAILHOUSE_ARCH) \
		CROSS_COMPILE=$(CROSS_COMPILE) \
		KDIR=$(abspath $(KDIR)) \
		VJ_DIR=
	@rm -f "$(INTEGRATED_MODE_STAMP)"
	@touch "$(ORIGINAL_MODE_STAMP)" "$@"

jailhouse-original: $(ORIGINAL_MODE_STAMP)


$(JAILHOUSE_BUILD_STAMP): $(JAILHOUSE_SOURCE_FILES) $(VJ_SOURCE_FILES) \
		$(JAILHOUSE_RUST_OBJ) | jailhouse-check-env
	+$(JAILHOUSE_MAKE) -C $(JAILHOUSE_DIR) clean \
		ARCH=$(JAILHOUSE_ARCH) \
		CROSS_COMPILE=$(CROSS_COMPILE) \
		KDIR=$(abspath $(KDIR)) \
		VJ_DIR=
	$(JAILHOUSE_MAKE) -C $(JAILHOUSE_DIR) modules \
		ARCH=$(JAILHOUSE_ARCH) \
		CROSS_COMPILE=$(CROSS_COMPILE) \
		KDIR=$(abspath $(KDIR)) \
		VJ_DIR=$(CURDIR) \
		VJ_FRAME_POOL_PAGES=$(VJ_FRAME_POOL_PAGES)
	@rm -f "$(ORIGINAL_MODE_STAMP)"
	@touch "$(INTEGRATED_MODE_STAMP)" "$@"

jailhouse-build: $(INTEGRATED_MODE_STAMP)

jailhouse-audit: jailhouse-build
	@test -f $(JAILHOUSE_HYPERVISOR_OBJ) || \
		{ echo "missing Jailhouse hypervisor object: $(JAILHOUSE_HYPERVISOR_OBJ)" >&2; exit 1; }
	@test -s $(JAILHOUSE_BIN) || \
		{ echo "missing or empty Jailhouse binary: $(JAILHOUSE_BIN)" >&2; exit 1; }
	@for symbol in $(VJ_LINKED_SYMBOLS); do \
		if ! $(JAILHOUSE_NM) -g --defined-only $(JAILHOUSE_HYPERVISOR_OBJ) | \
			$(GREP) -Eq "[[:space:]][TtWw][[:space:]]$$symbol$$"; then \
			echo "missing linked VeriHyMem symbol $$symbol in $(JAILHOUSE_HYPERVISOR_OBJ)" >&2; \
			exit 1; \
		fi; \
	done
	@if $(JAILHOUSE_NM) -u $(JAILHOUSE_HYPERVISOR_OBJ) | $(GREP) -q '[^[:space:]]'; then \
		echo "unresolved symbols in $(JAILHOUSE_HYPERVISOR_OBJ):" >&2; \
		$(JAILHOUSE_NM) -u $(JAILHOUSE_HYPERVISOR_OBJ) >&2; \
		exit 1; \
	fi
	@if $(JAILHOUSE_NM) $(JAILHOUSE_HYPERVISOR_OBJ) | \
		$(GREP) -Eq 'rust_eh_personality|_Unwind_'; then \
		echo "unexpected unwinding support in $(JAILHOUSE_HYPERVISOR_OBJ)" >&2; \
		exit 1; \
	fi
	@unexpected="$$( \
		$(JAILHOUSE_OBJDUMP) -h $(JAILHOUSE_HYPERVISOR_OBJ) | $(AWK) ' \
			/^[[:space:]]*[0-9]+[[:space:]]/ { \
				name = $$2; \
				if (name ~ /^\.(text|rodata|data|bss)\./ || \
				    name ~ /^\.(got|sdata|sbss|tdata|tbss)(\.|$$)/) \
					print name; \
			}' \
	)"; \
	if [ -n "$$unexpected" ]; then \
		echo "unexpected allocatable or orphan section families in $(JAILHOUSE_HYPERVISOR_OBJ):" >&2; \
		echo "$$unexpected" >&2; \
		exit 1; \
	fi
	@$(JAILHOUSE_NM) -n $(JAILHOUSE_HYPERVISOR_OBJ) | $(AWK) ' \
		$$3 == "__page_pool" { pool_seen = 1; next } \
		pool_seen && $$3 ~ /^vj_/ { symbol_after_pool = 1 } \
		END { exit !(pool_seen && !symbol_after_pool) }' || \
		{ echo "VeriHyMem symbols are not wholly located before __page_pool" >&2; exit 1; }
	@$(JAILHOUSE_READELF) -h $(JAILHOUSE_HYPERVISOR_OBJ) | \
		$(GREP) -q 'Machine:.*AArch64' || \
		{ echo "unexpected Jailhouse hypervisor object type" >&2; \
		  $(JAILHOUSE_READELF) -h $(JAILHOUSE_HYPERVISOR_OBJ) >&2; exit 1; }
	@size="$$( $(WC) -c < $(JAILHOUSE_BIN) | $(AWK) '{ print $$1 }' )"; \
		echo "linked $(JAILHOUSE_BIN) ($$size bytes)"

jailhouse-link: jailhouse-audit

jailhouse-integrated: jailhouse-link

install-jailhouse-artifacts:
	@test -n "$(DEST_ROOTFS)" || \
		{ echo "DEST_ROOTFS is required" >&2; exit 1; }
	@test -f "$(DEST_ROOTFS)/var/lib/dpkg/status" || \
		{ echo "DEST_ROOTFS is not a prepared rootfs: $(DEST_ROOTFS)" >&2; exit 1; }
	@for artifact in "$(JAILHOUSE_TOOL)" "$(JAILHOUSE_BIN)" \
		"$(JAILHOUSE_KO)" "$(JAILHOUSE_SYSTEM_CELL)" \
		"$(JAILHOUSE_INMATE_CELL)" "$(JAILHOUSE_INMATE_BIN)"; do \
		test -f "$$artifact" || \
			{ echo "missing Jailhouse artifact: $$artifact" >&2; exit 1; }; \
	done
	$(SUDO) install -Dm755 "$(JAILHOUSE_TOOL)" \
		"$(DEST_ROOTFS)/usr/local/sbin/jailhouse"
	$(SUDO) install -Dm644 "$(JAILHOUSE_BIN)" \
		"$(DEST_ROOTFS)/lib/firmware/jailhouse.bin"
	$(SUDO) install -Dm644 "$(JAILHOUSE_KO)" \
		"$(DEST_ROOTFS)/root/jailhouse/jailhouse.ko"
	$(SUDO) install -Dm644 "$(JAILHOUSE_SYSTEM_CELL)" \
		"$(DEST_ROOTFS)/root/jailhouse/qemu-arm64.cell"
	$(SUDO) install -Dm644 "$(JAILHOUSE_INMATE_CELL)" \
		"$(DEST_ROOTFS)/root/jailhouse/qemu-arm64-inmate-demo.cell"
	$(SUDO) install -Dm755 "$(JAILHOUSE_INMATE_BIN)" \
		"$(DEST_ROOTFS)/root/jailhouse/gic-demo.bin"

$(ORIGINAL_ARTIFACTS_STAMP): $(ROOTFS_STAMP) $(ORIGINAL_BUILD_STAMP)
	+$(MAKE) jailhouse-original
	+$(MAKE) install-jailhouse-artifacts DEST_ROOTFS="$(ORIGINAL_ROOTFS_DIR)"
	$(SUDO) touch "$(ORIGINAL_ARTIFACTS_STAMP)"
	@echo "installed original Jailhouse artifacts in $(ORIGINAL_ROOTFS_DIR)"

refresh-original-rootfs: $(ORIGINAL_ARTIFACTS_STAMP)

original-rootfs: $(ORIGINAL_ARTIFACTS_STAMP)

$(INTEGRATED_ROOTFS_COPY_STAMP): $(ORIGINAL_ARTIFACTS_STAMP)
	@test "$(abspath $(INTEGRATED_ROOTFS_DIR))" != \
		"$(abspath $(ORIGINAL_ROOTFS_DIR))" || \
		{ echo "original and integrated rootfs paths must differ" >&2; exit 1; }
	@if test -f "$(INTEGRATED_ROOTFS_DIR)/var/lib/dpkg/status"; then \
		echo "using existing integrated rootfs tree: $(INTEGRATED_ROOTFS_DIR)"; \
	else \
		test ! -e "$(INTEGRATED_ROOTFS_DIR)" || { \
			echo "refusing non-rootfs path: $(INTEGRATED_ROOTFS_DIR)" >&2; \
			exit 1; \
		}; \
		$(SUDO) cp -a --reflink=auto \
			"$(ORIGINAL_ROOTFS_DIR)" "$(INTEGRATED_ROOTFS_DIR)"; \
	fi
	$(SUDO) touch "$@"
	@echo "copied original rootfs to $(INTEGRATED_ROOTFS_DIR)"

$(INTEGRATED_ROOTFS_STAMP): $(INTEGRATED_ROOTFS_COPY_STAMP) $(JAILHOUSE_BUILD_STAMP)
	+$(MAKE) jailhouse-integrated
	+$(MAKE) install-jailhouse-artifacts DEST_ROOTFS="$(INTEGRATED_ROOTFS_DIR)"
	$(SUDO) touch "$(INTEGRATED_ROOTFS_STAMP)"
	@echo "installed integrated Jailhouse artifacts in $(INTEGRATED_ROOTFS_DIR)"

integrated-rootfs: $(INTEGRATED_ROOTFS_STAMP)

$(ORIGINAL_DISK_IMAGE): $(ORIGINAL_ARTIFACTS_STAMP)
	@mkdir -p "$(dir $(ORIGINAL_DISK_IMAGE))"
	@set -e; tmp="$(ORIGINAL_DISK_IMAGE).tmp"; \
		test ! -e "$$tmp" || { echo "stale temporary image: $$tmp" >&2; exit 1; }; \
		trap 'rm -f "$$tmp"' EXIT HUP INT TERM; \
		truncate -s "$(DISK_SIZE)" "$$tmp"; \
		$(SUDO) $(MKFS_EXT4) -F -L jailhouse-root \
			-d "$(ORIGINAL_ROOTFS_DIR)" "$$tmp"; \
		mv -f "$$tmp" "$(ORIGINAL_DISK_IMAGE)"; \
		trap - EXIT HUP INT TERM
	@echo "refreshed original image: $(ORIGINAL_DISK_IMAGE)"

original-image: $(ORIGINAL_DISK_IMAGE)

$(INTEGRATED_DISK_IMAGE): $(INTEGRATED_ROOTFS_STAMP)
	@mkdir -p "$(dir $(INTEGRATED_DISK_IMAGE))"
	@set -e; tmp="$(INTEGRATED_DISK_IMAGE).tmp"; \
		test ! -e "$$tmp" || { echo "stale temporary image: $$tmp" >&2; exit 1; }; \
		trap 'rm -f "$$tmp"' EXIT HUP INT TERM; \
		truncate -s "$(DISK_SIZE)" "$$tmp"; \
		$(SUDO) $(MKFS_EXT4) -F -L jailhouse-root \
			-d "$(INTEGRATED_ROOTFS_DIR)" "$$tmp"; \
		mv -f "$$tmp" "$(INTEGRATED_DISK_IMAGE)"; \
		trap - EXIT HUP INT TERM
	@echo "refreshed integrated image: $(INTEGRATED_DISK_IMAGE)"

integrated-image: $(INTEGRATED_DISK_IMAGE)

qemu-check:
	@command -v "$(QEMU)" >/dev/null 2>&1 || \
		{ echo "QEMU executable not found: $(QEMU)" >&2; exit 1; }
	@test -f "$(KERNEL_IMAGE)" || \
		{ echo "kernel Image not found: $(KERNEL_IMAGE); set KDIR or KERNEL_IMAGE" >&2; exit 1; }

run-original: original-image qemu-check
	$(QEMU) \
		-cpu "$(QEMU_CPU)" -smp "$(QEMU_CPUS)" -m "$(QEMU_RAM)" \
		-machine virt,gic-version=3,virtualization=on,its=off \
		-nographic \
		-netdev user,id=net -device virtio-net-device,netdev=net \
		-drive file="$(ORIGINAL_DISK_IMAGE)",format=raw,id=disk,if=none \
		-device virtio-blk-device,drive=disk \
		-kernel "$(KERNEL_IMAGE)" \
		-append "$(QEMU_APPEND)" \
		$(QEMU_EXTRA_ARGS)

run-integrated: integrated-image qemu-check
	$(QEMU) \
		-cpu "$(QEMU_CPU)" -smp "$(QEMU_CPUS)" -m "$(QEMU_RAM)" \
		-machine virt,gic-version=3,virtualization=on,its=off \
		-nographic \
		-netdev user,id=net -device virtio-net-device,netdev=net \
		-drive file="$(INTEGRATED_DISK_IMAGE)",format=raw,id=disk,if=none \
		-device virtio-blk-device,drive=disk \
		-kernel "$(KERNEL_IMAGE)" \
		-append "$(QEMU_APPEND)" \
		$(QEMU_EXTRA_ARGS)

test:
	RUST_MIN_STACK="$(TEST_STACK_SIZE)" RUSTFLAGS="$(TEST_RUSTFLAGS)" \
		$(CARGO) test --locked --lib
	$(HOST_CC) -std=c11 -Wall -Wextra -Werror -fsyntax-only \
		-x c /dev/null -include include/vj.h

clean:
	$(CARGO) clean
	@if test -n "$(KDIR)"; then \
		$(MAKE) jailhouse-clean; \
	else \
		echo "KDIR not set; skipped Jailhouse clean"; \
	fi
