// SPDX-License-Identifier: GPL-2.0
/*
 * emco_pl_overlay: apply one device-tree overlay from the firmware path when
 * loaded, and remove it when unloaded.
 *
 * Mainline Linux has no userspace interface for DT overlays (the configfs
 * one is an out-of-tree patch), but the FPGA region driver programs the PL
 * when an overlay naming a bitstream (firmware-name) is applied to its node,
 * and removing the overlay tears it down again. This module is that apply
 * and remove, and nothing else; kd240/nix/pl.nix loads it from a systemd
 * unit with the overlay the NixOS configuration built.
 *
 *   modprobe emco_pl_overlay overlay=emco-pl.dtbo
 *   modprobe -r emco_pl_overlay
 */
#include <linux/firmware.h>
#include <linux/module.h>
#include <linux/of.h>

static char *overlay;
module_param(overlay, charp, 0444);
MODULE_PARM_DESC(overlay, "overlay (.dtbo) file name, looked up like firmware");

static int ovcs_id;

static int __init emco_pl_overlay_init(void)
{
	const struct firmware *fw;
	int ret;

	if (!overlay || !*overlay) {
		pr_err("emco_pl_overlay: no overlay= given\n");
		return -EINVAL;
	}
	ret = request_firmware(&fw, overlay, NULL);
	if (ret) {
		pr_err("emco_pl_overlay: %s: not found (%d)\n", overlay, ret);
		return ret;
	}
	/* The overlay code keeps its own copy of the blob. */
	ret = of_overlay_fdt_apply(fw->data, fw->size, &ovcs_id, NULL);
	release_firmware(fw);
	if (ret) {
		pr_err("emco_pl_overlay: %s: apply failed (%d)\n", overlay, ret);
		/* A partly applied changeset still has an id to undo. */
		if (ovcs_id)
			of_overlay_remove(&ovcs_id);
		return ret;
	}
	pr_info("emco_pl_overlay: %s applied (changeset %d)\n", overlay, ovcs_id);
	return 0;
}

static void __exit emco_pl_overlay_exit(void)
{
	int ret = of_overlay_remove(&ovcs_id);

	if (ret)
		pr_err("emco_pl_overlay: remove failed (%d)\n", ret);
	else
		pr_info("emco_pl_overlay: %s removed\n", overlay);
}

module_init(emco_pl_overlay_init);
module_exit(emco_pl_overlay_exit);
MODULE_DESCRIPTION("Apply a DT overlay (FPGA region + bitstream) from the firmware path");
MODULE_LICENSE("GPL");
