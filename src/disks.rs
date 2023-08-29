use crate::errors::AliError;
use crate::linux;
use crate::manifest;

pub fn do_disks(disks: &[manifest::ManifestDisk]) -> Result<(), AliError> {
    for disk in disks.iter() {
        do_disk(disk)?;
    }

    Ok(())
}

fn do_disk(disk: &manifest::ManifestDisk) -> Result<(), AliError> {
    let cmd_create_table = linux::fdisk::create_table_cmd(&disk.device, &disk.table);
    linux::fdisk::run_fdisk_cmd(&disk.device, &cmd_create_table)?;

    for (n, part) in disk.partitions.iter().enumerate() {
        let cmd_create_part = linux::fdisk::create_partition_cmd(&disk.table, n + 1, part);

        linux::fdisk::run_fdisk_cmd(&disk.device, &cmd_create_part)?;
    }

    Ok(())
}