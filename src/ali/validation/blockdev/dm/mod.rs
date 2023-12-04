mod lv;

use std::collections::{
    HashMap,
    LinkedList,
};

use crate::ali::validation::*;
use crate::ali::{
    self,
    Dm,
    ManifestLuks,
    ManifestLvmLv,
    ManifestLvmVg,
};
use crate::entity::blockdev::*;
use crate::errors::AliError;

pub(super) fn collect_valids(
    dms: &[Dm],
    sys_fs_devs: &HashMap<String, BlockDevType>,
    sys_fs_ready_devs: &mut HashMap<String, BlockDevType>,
    sys_lvms: &mut HashMap<String, BlockDevPaths>,
    valids: &mut BlockDevPaths,
) -> Result<(), AliError> {
    // Validate sizing of LVs
    // Only the last LV on each VG could be unsized (100%FREE)
    validate_lv_size(dms)?;

    // Collect all DMs into valids to be used later in filesystems validation
    for dm in dms {
        match dm {
            Dm::Luks(luks) => {
                // Appends LUKS to a path in valids, if OK
                collect_valid_luks(
                    luks,
                    sys_fs_devs,
                    sys_fs_ready_devs,
                    sys_lvms,
                    valids,
                )?;
            }

            // We validate a LVM manifest block by adding valid devices in these exact order:
            // PV -> VG -> LV
            // This gives us certainty that during VG validation, any known PV would have been in valids.
            Dm::Lvm(lvm) => {
                if let Some(pvs) = &lvm.pvs {
                    for pv_path in pvs {
                        // Appends PV to a path in valids, if OK
                        collect_valid_pv(
                            pv_path,
                            sys_fs_devs,
                            sys_fs_ready_devs,
                            sys_lvms,
                            valids,
                        )?;
                    }
                }

                if let Some(vgs) = &lvm.vgs {
                    for vg in vgs {
                        // Appends VG to paths in valids, if OK
                        collect_valid_vg(vg, sys_fs_devs, sys_lvms, valids)?;
                    }
                }

                if let Some(lvs) = &lvm.lvs {
                    for lv in lvs {
                        // Appends LV to paths in valids, if OK
                        lv::collect_valid(lv, sys_fs_devs, sys_lvms, valids)?;
                    }
                }
            }
        }
    }

    Ok(())
}

// Only the last LV on each VG could be unsized
// (uses 100% of the remaining space)
#[inline]
fn validate_lv_size(dms: &[ali::Dm]) -> Result<(), AliError> {
    // Collect VG -> LVs
    let mut vg_lvs: HashMap<String, Vec<ManifestLvmLv>> = HashMap::new();
    for dm in dms {
        if let ali::Dm::Lvm(lvm) = dm {
            if lvm.lvs.is_none() {
                continue;
            }

            let lvs = lvm.lvs.as_ref().unwrap();
            for lv in lvs {
                // Check if size string is valid
                if let Some(ref size) = lv.size {
                    if let Err(err) = parse_human_bytes(size) {
                        return Err(AliError::BadManifest(format!(
                            "bad lv size {size}: {err}"
                        )));
                    }
                }

                if vg_lvs.contains_key(&lv.vg) {
                    vg_lvs.get_mut(&lv.vg).unwrap().push(lv.clone());
                    continue;
                }

                vg_lvs.insert(lv.vg.clone(), vec![lv.clone()]);
            }
        }
    }

    for (vg, lvs) in vg_lvs.into_iter() {
        if lvs.is_empty() {
            continue;
        }

        let l = lvs.len();
        if l == 1 {
            continue;
        }

        for (i, lv) in lvs.into_iter().enumerate() {
            if lv.size.is_none() && (i != l - 1) {
                return Err(AliError::BadManifest(format!(
                    "lv {} on vg {vg} has None size",
                    lv.name
                )));
            }
        }
    }

    Ok(())
}

// Collects valid block device path(s) into valids
#[inline]
fn collect_valid_luks(
    luks: &ManifestLuks,
    sys_fs_devs: &HashMap<String, BlockDevType>,
    sys_fs_ready_devs: &mut HashMap<String, BlockDevType>,
    sys_lvms: &mut HashMap<String, BlockDevPaths>,
    valids: &mut BlockDevPaths,
) -> Result<(), AliError> {
    let (luks_base_path, luks_path) =
        (&luks.device, format!("/dev/mapper/{}", luks.name));

    let msg = "dm luks validation failed";
    if file_exists(&luks_path) {
        return Err(AliError::BadManifest(format!(
            "{msg}: device {luks_path} already exists"
        )));
    }

    if let Some(fs_type) = sys_fs_devs.get(luks_base_path) {
        return Err(AliError::BadManifest(format!(
            "{msg}: luks {} base {luks_base_path} was already in use as {fs_type}",
            luks.name
        )));
    }

    let mut found_vg: Option<BlockDev> = None;

    // Find base LV and its VG in existing LVMs
    'find_some_vg: for (lvm_base, sys_lvm_lists) in sys_lvms.iter() {
        for sys_lvm in sys_lvm_lists {
            let top_most = sys_lvm.back();

            if top_most.is_none() {
                continue;
            }

            let top_most = top_most.unwrap();
            if top_most.device.as_str() != luks_base_path {
                continue;
            }

            if !is_luks_base(&top_most.device_type) {
                return Err(AliError::BadManifest(format!(
                    "{msg}: luks base {} (itself is an LVM from {}) cannot have type {}",
                    luks_base_path, lvm_base, top_most.device_type
                )));
            }

            // We could really use unstable Cursor type here
            // See also: https://doc.rust-lang.org/std/collections/linked_list/struct.Cursor.html
            let mut path = sys_lvm.clone();
            path.pop_back();
            let should_be_vg = path.pop_back().expect("no vg after 2 pops");

            if should_be_vg.device_type != TYPE_VG {
                return Err(AliError::AliRsBug(format!(
                    "{msg}: unexpected device type {} - expecting a VG",
                    should_be_vg.device_type,
                )));
            }

            found_vg = Some(should_be_vg);
            break 'find_some_vg;
        }
    }

    let luks_dev = BlockDev {
        device: luks_path,
        device_type: TYPE_LUKS,
    };

    // Although a LUKS can only sit on 1 LV,
    // We keep pushing since an LV may sit on VG with >1 PVs
    if let Some(vg) = found_vg {
        // Push all paths leading to VG and LV
        'new_pv: for sys_lvm_lists in sys_lvms.values_mut() {
            for sys_lvm in sys_lvm_lists.iter_mut() {
                let top_most = sys_lvm.back();

                if top_most.is_none() {
                    continue;
                }

                // Check if this path contains our VG -> LV
                let top_most = top_most.unwrap();
                if top_most.device.as_str() != luks_base_path {
                    continue;
                }

                let mut tmp_path = sys_lvm.clone();
                tmp_path.pop_back();
                let maybe_vg = tmp_path.pop_back().expect("no vg after 2 pops");

                if maybe_vg.device_type != TYPE_VG {
                    return Err(AliError::AliRsBug(format!(
                        "{msg}: unexpected device type {} - expecting a VG",
                        maybe_vg.device_type,
                    )));
                }

                if maybe_vg.device.as_str() != vg.device {
                    continue;
                }

                let mut list = sys_lvm.clone();
                list.push_back(luks_dev.clone());
                valids.push(list);
                sys_lvm.clear();

                continue 'new_pv;
            }
        }

        return Ok(());
    }

    // Find base device for LUKS
    // There's a possibility that LUKS sits on manifest LV on some VG
    // with itself having >1 PVs
    let mut found = false;
    for list in valids.iter_mut() {
        let top_most = list.back().expect("no back node in linked list in v");

        if top_most.device.as_str() != luks_base_path {
            continue;
        }

        if !is_luks_base(&top_most.device_type) {
            return Err(AliError::BadManifest(format!(
                "{msg}: luks {} base {luks_base_path} cannot have type {}",
                luks.name, top_most.device_type,
            )));
        }

        found = true;
        list.push_back(luks_dev.clone());
    }

    if found {
        return Ok(());
    }

    let unknown_base = BlockDev {
        device: luks_base_path.clone(),
        device_type: TYPE_UNKNOWN,
    };

    if sys_fs_ready_devs.contains_key(luks_base_path) {
        valids.push(LinkedList::from([unknown_base, luks_dev]));

        // Clear used up sys fs_ready device
        sys_fs_ready_devs.remove(luks_base_path);

        return Ok(());
    }

    // TODO: This may introduce error if such file is not a proper block device.
    if !file_exists(luks_base_path) {
        return Err(AliError::NoSuchDevice(luks_base_path.to_string()));
    }

    valids.push(LinkedList::from([unknown_base, luks_dev]));

    Ok(())
}

// Collect valid PV device path into valids
#[inline]
fn collect_valid_pv(
    pv_path: &str,
    sys_fs_devs: &HashMap<String, BlockDevType>,
    sys_fs_ready_devs: &mut HashMap<String, BlockDevType>,
    sys_lvms: &mut HashMap<String, BlockDevPaths>,
    valids: &mut BlockDevPaths,
) -> Result<(), AliError> {
    let msg = "lvm pv validation failed";
    if let Some(fs_type) = sys_fs_devs.get(pv_path) {
        return Err(AliError::BadManifest(format!(
            "{msg}: pv {pv_path} base was already used as {fs_type}",
        )));
    }

    // Find and invalidate duplicate PV if it was used for other VG
    if let Some(sys_pv_lvms) = sys_lvms.get(pv_path) {
        for node in sys_pv_lvms.iter().flatten() {
            if node.device_type != TYPE_VG {
                continue;
            }

            return Err(AliError::BadManifest(format!(
                "{msg}: pv {pv_path} was already used for other vg {}",
                node.device,
            )));
        }
    }

    // Find PV base from top-most values in v
    for list in valids.iter_mut() {
        let top_most = list
            .back()
            .expect("no back node in linked list from manifest_devs");

        if top_most.device.as_str() != pv_path {
            continue;
        }

        if top_most.device_type == TYPE_PV {
            return Err(AliError::BadManifest(format!(
                "{msg}: duplicate pv {pv_path} in manifest"
            )));
        }

        if !is_pv_base(&top_most.device_type) {
            return Err(AliError::BadManifest(format!(
                "{msg}: pv {} base cannot have type {}",
                pv_path, top_most.device_type,
            )));
        }

        list.push_back(BlockDev {
            device: pv_path.to_string(),
            device_type: TYPE_PV,
        });

        return Ok(());
    }

    // Check if PV base device is in sys_fs_ready_devs
    if sys_fs_ready_devs.contains_key(pv_path) {
        // Add both base and PV
        valids.push(LinkedList::from([
            BlockDev {
                device: pv_path.to_string(),
                device_type: TYPE_UNKNOWN,
            },
            BlockDev {
                device: pv_path.to_string(),
                device_type: TYPE_PV,
            },
        ]));

        // Removed used up sys fs_ready device
        sys_fs_ready_devs.remove(pv_path);
        return Ok(());
    }

    // TODO: This may introduce error if such file is not a proper block device.
    if !file_exists(pv_path) {
        return Err(AliError::BadManifest(format!(
            "{msg}: no such pv device: {pv_path}"
        )));
    }

    valids.push(LinkedList::from([
        BlockDev {
            device: pv_path.to_string(),
            device_type: TYPE_UNKNOWN,
        },
        BlockDev {
            device: pv_path.to_string(),
            device_type: TYPE_PV,
        },
    ]));

    Ok(())
}

// Collect valid VG device path into valids
#[inline]
fn collect_valid_vg(
    vg: &ManifestLvmVg,
    sys_fs_devs: &HashMap<String, BlockDevType>,
    sys_lvms: &mut HashMap<String, BlockDevPaths>,
    valids: &mut BlockDevPaths,
) -> Result<(), AliError> {
    let vg_dev = BlockDev {
        device: format!("/dev/{}", vg.name),
        device_type: TYPE_VG,
    };

    let msg = "lvm vg validation failed";
    'validate_vg_pv: for pv_base in &vg.pvs {
        // Invalidate VG if its PV was already used as FS partition
        if let Some(fs) = sys_fs_devs.get(pv_base) {
            return Err(AliError::BadManifest(format!(
                "{msg}: vg {} base {} was already used as filesystem {fs}",
                vg.name, pv_base
            )));
        }

        // Invalidate VG if its PV was already used in sys LVM
        if let Some(sys_pv_lvms) = sys_lvms.get(pv_base) {
            for node in sys_pv_lvms.iter().flatten() {
                if node.device_type != TYPE_VG {
                    continue;
                }

                return Err(AliError::BadManifest(format!(
                    "{msg}: vg {} base {} was already used for other vg {}",
                    vg.name, pv_base, node.device,
                )));
            }
        }

        // Check if top-most device is PV
        for list in valids.iter_mut() {
            let top_most = list
                .back()
                .expect("no back node in linked list from manifest_devs");

            if top_most.device.as_str() != pv_base {
                continue;
            }

            if !is_vg_base(&top_most.device_type) {
                return Err(AliError::BadManifest(format!(
                    "{msg}: vg {} pv base {pv_base} cannot have type {}",
                    vg.name, top_most.device_type,
                )));
            }

            list.push_back(vg_dev.clone());

            continue 'validate_vg_pv;
        }

        // Find sys_lvm PV to base on
        for sys_lvm_lists in sys_lvms.values_mut() {
            for sys_lvm in sys_lvm_lists {
                let top_most = sys_lvm.back();

                if top_most.is_none() {
                    continue;
                }

                let top_most = top_most.unwrap();
                if *top_most == vg_dev {
                    return Err(AliError::BadManifest(format!(
                        "{msg}: vg {} already exists",
                        vg.name,
                    )));
                }

                if top_most.device.as_str() != pv_base {
                    continue;
                }

                if !is_vg_base(&top_most.device_type) {
                    return Err(AliError::BadManifest(format!(
                        "{msg}: vg {} pv base {pv_base} cannot have type {}",
                        vg.name, top_most.device_type
                    )));
                }

                let mut new_list = sys_lvm.clone();
                new_list.push_back(vg_dev.clone());

                // Push to valids, and remove used up sys_lvms path
                valids.push(new_list);
                sys_lvm.clear();

                continue 'validate_vg_pv;
            }
        }

        return Err(AliError::BadManifest(format!(
            "{msg}: no pv device matching {pv_base} in manifest or in the system"
        )));
    }

    Ok(())
}

#[inline(always)]
fn vg_lv_name(lv: &ManifestLvmLv) -> (String, String) {
    let vg_name = if lv.vg.contains("/dev/") {
        lv.vg.clone()
    } else {
        format!("/dev/{}", lv.vg)
    };

    (vg_name.clone(), format!("{vg_name}/{}", lv.name))
}

#[inline(always)]
fn is_luks_base(dev_type: &BlockDevType) -> bool {
    matches!(
        dev_type,
        BlockDevType::UnknownBlock
            | BlockDevType::Disk
            | BlockDevType::Partition
            | BlockDevType::Dm(DmType::LvmLv)
    )
}

#[inline(always)]
fn is_pv_base(dev_type: &BlockDevType) -> bool {
    matches!(
        dev_type,
        BlockDevType::UnknownBlock
            | BlockDevType::Disk
            | BlockDevType::Partition
            | BlockDevType::Dm(DmType::Luks)
    )
}

#[inline(always)]
fn is_vg_base(dev_type: &BlockDevType) -> bool {
    matches!(dev_type, BlockDevType::Dm(DmType::LvmPv))
}

#[inline(always)]
fn is_lv_base(dev_type: &BlockDevType) -> bool {
    matches!(dev_type, BlockDevType::Dm(DmType::LvmVg))
}