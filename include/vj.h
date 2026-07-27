#ifndef VJ_H
#define VJ_H

#include <stddef.h>
#include <stdint.h>

struct cell;
struct jailhouse_memory;

/* Jailhouse integration entry points implemented by hypervisor/vj.c. */
int vj_paging_init(void);
int vj_cell_init(struct cell *cell);
int vj_cell_map_memory_region(struct cell *cell,
			      const struct jailhouse_memory *mem,
			      uintptr_t phys_start);
int vj_cell_unmap_memory_region(struct cell *cell,
				const struct jailhouse_memory *mem);
int vj_cell_destroy(struct cell *cell);

/*
 * The frame pool is dedicated to VeriHyMem's GlobalFrameAllocator and must not
 * overlap JailhouseHeapAllocator storage. Pool exhaustion is outside the
 * prototype contract and may abort rather than return an allocation error.
 */
int32_t vj_global_frame_allocator_init(
						uintptr_t frame_pool_hva_base,
						uintptr_t frame_pool_frame_count,
						uintptr_t hva_to_pa_offset);

struct vj_map_attrs {
	uint8_t readable;
	uint8_t writable;
	uint8_t executable;
	uint8_t device;
};

struct vj_mapping {
	uintptr_t ipa_base;
	uintptr_t pa_base;
	uintptr_t size;
	struct vj_map_attrs attrs;
};

int32_t vj_pt_create(uintptr_t frame_pool_hva_base,
						     uintptr_t frame_pool_frame_count,
						     uintptr_t hva_to_pa_offset,
						     uint8_t ipa_bits,
						     void **out_handle);
int32_t vj_pt_map_page(
					void *handle, uintptr_t ipa, uintptr_t pa,
					struct vj_map_attrs attrs);
int32_t vj_pt_unmap_page(void *handle, uintptr_t ipa);
int32_t vj_pt_query(
					const void *handle, uintptr_t ipa,
					struct vj_mapping *out);
int32_t vj_pt_root_pa(const void *handle,
					       uintptr_t *out_root_pa);
int32_t vj_pt_mapped_pages(const void *handle,
						    uintptr_t *out_mapped_pages);
int32_t vj_pt_destroy(void *handle);

/* Rust heap hooks; independent of VeriHyMem's GlobalFrameAllocator. */
void *vj_heap_alloc(size_t size, size_t align);
void vj_heap_dealloc(void *ptr, size_t size, size_t align);
_Noreturn void vj_abort(void);

#endif /* VJ_H */
