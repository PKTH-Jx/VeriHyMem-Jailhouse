#ifndef VERIHYMEM_JAILHOUSE_H
#define VERIHYMEM_JAILHOUSE_H

#include <stddef.h>
#include <stdint.h>

#define VERIHYMEM_JAILHOUSE_ABI_VERSION 1U

uint32_t verihymem_jailhouse_abi_version(void);

int32_t verihymem_jailhouse_mem_pool_init(uintptr_t table_hva_base,
						  uintptr_t table_frame_count,
						  uintptr_t hva_to_pa_offset);

struct verihymem_jailhouse_map_attrs {
	uint8_t readable;
	uint8_t writable;
	uint8_t executable;
	uint8_t device;
};

struct verihymem_jailhouse_mapping {
	uintptr_t ipa_base;
	uintptr_t pa_base;
	uintptr_t size;
	struct verihymem_jailhouse_map_attrs attrs;
};

void *verihymem_jailhouse_pt_create(uintptr_t table_hva_base,
					    uintptr_t table_frame_count,
					    uintptr_t hva_to_pa_offset,
					    uint8_t ipa_bits);
int32_t verihymem_jailhouse_pt_map_page(
					void *handle, uintptr_t ipa, uintptr_t pa,
					struct verihymem_jailhouse_map_attrs attrs);
int32_t verihymem_jailhouse_pt_unmap_page(void *handle, uintptr_t ipa);
int32_t verihymem_jailhouse_pt_query(
					const void *handle, uintptr_t ipa,
					struct verihymem_jailhouse_mapping *out);
int32_t verihymem_jailhouse_pt_destroy(void *handle);

/* Runtime hooks that Jailhouse must provide when linking the Rust staticlib. */
void *verihymem_jailhouse_alloc(size_t size, size_t align);
void verihymem_jailhouse_dealloc(void *ptr, size_t size, size_t align);
_Noreturn void verihymem_jailhouse_abort(void);

#endif /* VERIHYMEM_JAILHOUSE_H */
