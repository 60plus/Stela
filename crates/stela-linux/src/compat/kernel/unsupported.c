// SPDX-License-Identifier: GPL-2.0-or-later
/* Kernel services Stela does not provide yet. */
#include <linux/folio_batch.h>
#include <linux/fs.h>
#include <linux/hmm.h>
#include <linux/i2c.h>
#include <linux/io.h>
#include <linux/kthread.h>
#include <linux/mm.h>
#include <linux/platform_device.h>
#include <linux/property.h>
#include <linux/rcupdate.h>
#include <linux/regmap.h>
#include <linux/sched/mm.h>
#include <linux/seq_file.h>
#include <linux/swap.h>
#include <stela/linux.h>
#include <unsupported.h>

/* Pages behind shmem files and shmem-backed GEM objects, which swap and
 * nouveau's TTM objects never need: TTM allocates pages from its own pool and
 * Stela never reclaims them. */
STELA_UNSUPPORTED(struct folio *, shmem_read_folio_gfp, struct address_space *, pgoff_t, gfp_t)
STELA_UNSUPPORTED(struct page *, shmem_read_mapping_page_gfp, struct address_space *, pgoff_t,
		  gfp_t)
STELA_UNSUPPORTED(void, shmem_truncate_range, struct inode *, loff_t, uoff_t)
struct swap_iocb;
STELA_UNSUPPORTED(int, shmem_writeout, struct folio *, struct swap_iocb **, struct list_head *)
STELA_UNSUPPORTED(bool, folio_clear_dirty_for_io, struct folio *)
/* Transparent huge mappings of shmem-backed objects. */
STELA_UNSUPPORTED(unsigned long, mm_get_unmapped_area, struct file *, unsigned long,
		  unsigned long, unsigned long, unsigned long)
/* The locks of every mapping of an address space, which subscribing its
 * first notifier takes. Only a driver mirroring a client's memory (userptr)
 * subscribes one, and that also needs the client's pages faulted in through
 * Linux page tables, which Stela's clients do not have. */
STELA_UNSUPPORTED(int, mm_take_all_locks, struct mm_struct *)
STELA_UNSUPPORTED(void, mm_drop_all_locks, struct mm_struct *)
/* A client's own memory as a device's buffer (userptr) and a value written
 * into a client from a worker (a user fence). Both need the client as a
 * Linux address space with VMAs and page tables. Stela's clients are not:
 * the Metal device, the only caller of a GPU driver's ioctls, asks for
 * neither. A fault of a range is refused and said once. A user fence is
 * refused by xe where it is asked for, with one warning: no Stela task has an
 * address space, and xe refuses a caller without one (third_party/patches/
 * linux, "refuse a user fence"). The rest is reached only after a range
 * faulted or a worker adopted an address space, so by nothing. */
int hmm_range_fault(struct hmm_range *range)
{
	(void)range;
	stela_linux_unsupported("a client's own memory as a device buffer (userptr)");
	return -EOPNOTSUPP;
}
STELA_UNSUPPORTED(struct vm_area_struct *, find_vma_intersection, struct mm_struct *,
		  unsigned long, unsigned long)
STELA_UNSUPPORTED(void, kthread_use_mm, struct mm_struct *)
STELA_UNSUPPORTED(void, kthread_unuse_mm, struct mm_struct *)
STELA_UNSUPPORTED(void, mmput, struct mm_struct *)
/* Register maps, which only a discrete Intel GPU's management controller
 * (an I2C controller behind the GPU's own registers) asks for. Stela has no
 * platform bus for that controller's driver, so the map is refused and said
 * once and the GPU's probe ends with that error; what follows a map (the
 * controller as a platform device with a software node, its adapter seen
 * through the I2C bus's notifier, transfers to the controller) is not
 * reached. */
struct regmap *__devm_regmap_init(struct device *dev, const struct regmap_bus *bus,
				  void *bus_context, const struct regmap_config *config,
				  struct lock_class_key *lock_key, const char *lock_name)
{
	(void)dev;
	(void)bus;
	(void)bus_context;
	(void)config;
	(void)lock_key;
	(void)lock_name;
	stela_linux_unsupported("register maps: a GPU's management controller");
	return ERR_PTR(-EOPNOTSUPP);
}
STELA_UNSUPPORTED(int, bus_register_notifier, const struct bus_type *, struct notifier_block *)
STELA_UNSUPPORTED(int, bus_unregister_notifier, const struct bus_type *,
		  struct notifier_block *)
STELA_UNSUPPORTED(struct fwnode_handle *, fwnode_create_software_node,
		  const struct property_entry *, const struct fwnode_handle *)
STELA_UNSUPPORTED(void, fwnode_remove_software_node, struct fwnode_handle *)
STELA_UNSUPPORTED(struct platform_device *, platform_device_alloc, const char *, int)
STELA_UNSUPPORTED(int, platform_device_add, struct platform_device *)
STELA_UNSUPPORTED(void, platform_device_put, struct platform_device *)
STELA_UNSUPPORTED_OBJECT(const struct bus_type, i2c_bus_type);
STELA_UNSUPPORTED(struct i2c_adapter *, i2c_verify_adapter, struct device *)
STELA_UNSUPPORTED(int, i2c_transfer_buffer_flags, const struct i2c_client *, char *, int, u16)
/* debugfs's seq_file operations, which no file opens: Stela has no debugfs. */
STELA_UNSUPPORTED(int, single_open, struct file *, int (*)(struct seq_file *, void *), void *)
STELA_UNSUPPORTED(int, single_release, struct inode *, struct file *)
STELA_UNSUPPORTED(ssize_t, seq_read, struct file *, char __user *, size_t, loff_t *)
STELA_UNSUPPORTED(loff_t, seq_lseek, struct file *, loff_t, int)
/* Platform devices, which need the platform bus: only HDMI audio codecs and
 * the partitions of a partitioned accelerator reach DRM as such. */
STELA_UNSUPPORTED(struct platform_device *, platform_device_register_full,
		  const struct platform_device_info *)
STELA_UNSUPPORTED(void, platform_device_unregister, struct platform_device *)
/* Whether a frame is RAM is the answer of the firmware's memory map in
 * iomem_resource, which holds no map here: Stela's owners hand Linux its
 * pages. Only the NUMA placement of an accelerator's memory partitions asks. */
STELA_UNSUPPORTED(int, page_is_ram, unsigned long)
