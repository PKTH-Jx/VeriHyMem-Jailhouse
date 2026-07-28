#ifndef VJ_H
#define VJ_H

#include <stddef.h>
#include <stdint.h>

struct cell;
struct jailhouse_memory;

#define VJ_IPA_BITS 39
#define VJ_FRAME_POOL_MAX_PAGES 4096

/* VJ's counterpart to Jailhouse's struct paging_structures. */
struct vj_paging_structures {
	void *root_table;
	uintptr_t root_table_pa;
	uint8_t ipa_bits;
};

/* Jailhouse integration entry points implemented by hypervisor/vj.c. */
int vj_paging_init(void);
int vj_cell_init(struct cell *cell);
int vj_cell_map_memory_region(struct cell *cell,
			      const struct jailhouse_memory *mem,
			      uintptr_t phys_start);
int vj_cell_unmap_memory_region(struct cell *cell,
				const struct jailhouse_memory *mem);
int vj_iommu_map_memory_region(struct cell *cell,
			       const struct jailhouse_memory *mem);
int vj_iommu_unmap_memory_region(struct cell *cell,
				 const struct jailhouse_memory *mem);
int vj_cell_gphys2phys(const struct cell *cell, uintptr_t gphys,
			uintptr_t *out_phys);
int vj_cell_destroy(struct cell *cell);

/* Install a VJ stage-2 table on the current CPU. */
void vj_paging_vcpu_init(const struct vj_paging_structures *pg_structs);

/* Make VJ tables visible to an IOMMU and return their translation geometry. */
void vj_paging_sync(void);
uint64_t vj_paging_vtcr(const struct vj_paging_structures *pg_structs);

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
