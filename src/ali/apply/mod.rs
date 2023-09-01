mod archchroot;
mod disks;
mod dm;
mod fs;
mod routine;

use std::collections::HashSet;

use crate::ali::Manifest;
use crate::errors::AliError;
use crate::run::apply::Action;
use crate::utils::shell;

// Use manifest to install a new system
pub fn apply_manifest(
    manifest: &Manifest,
    install_location: &str,
) -> Result<Vec<Action>, AliError> {
    let mut actions = Vec::new();

    // Format and partition disks
    if let Some(ref m_disks) = manifest.disks {
        match disks::apply_disks(m_disks) {
            Err(err) => {
                return Err(AliError::InstallError {
                    error: Box::new(err),
                    action_failed: Box::new(Action::ApplyDisks),
                    actions_performed: actions,
                })
            }
            Ok(actions_disks) => actions.extend(actions_disks),
        };
    }

    // Format and create device mappers
    if let Some(ref m_dms) = manifest.device_mappers {
        match dm::apply_dms(m_dms) {
            Err(err) => {
                return Err(AliError::InstallError {
                    error: Box::new(err),
                    action_failed: Box::new(Action::ApplyDms),
                    actions_performed: actions,
                })
            }
            Ok(actions_dms) => actions.extend(actions_dms),
        }
    }

    // Create rootfs
    match fs::apply_filesystem(&manifest.rootfs) {
        Err(err) => {
            return Err(AliError::InstallError {
                error: Box::new(err),
                action_failed: Box::new(Action::ApplyRootfs),
                actions_performed: actions,
            });
        }
        Ok(action_create_rootfs) => actions.push(action_create_rootfs),
    };

    // Create other filesystems
    if let Some(filesystems) = &manifest.filesystems {
        match fs::apply_filesystems(filesystems) {
            Err(err) => {
                return Err(AliError::InstallError {
                    error: Box::new(err),
                    action_failed: Box::new(Action::ApplyFilesystems),
                    actions_performed: actions,
                });
            }
            Ok(actions_create_filesystems) => {
                actions.extend(actions_create_filesystems);
            }
        }
    }

    // mkdir rootfs chroot mount
    match shell::exec("mkdir", &["-p", install_location]) {
        Err(err) => {
            return Err(AliError::InstallError {
                error: Box::new(err),
                action_failed: Box::new(Action::MkdirRootFs),
                actions_performed: actions,
            });
        }
        Ok(()) => actions.push(Action::MkdirRootFs),
    }

    // Mount rootfs
    match fs::mount_filesystem(&manifest.rootfs, install_location) {
        Err(err) => {
            return Err(AliError::InstallError {
                error: Box::new(err),
                action_failed: Box::new(Action::MountRootFs),
                actions_performed: actions,
            });
        }
        Ok(action_mount_rootfs) => actions.push(action_mount_rootfs),
    }

    // Mount other filesystems to /{DEFAULT_CHROOT_LOC}
    if let Some(filesystems) = &manifest.filesystems {
        // Collect filesystems mountpoints and actions.
        // The mountpoints will be prepended with default base
        let mountpoints: Vec<(String, Action)> = filesystems
            .iter()
            .filter_map(|fs| {
                fs.mnt.clone().map(|mountpoint| {
                    (
                        fs::prepend_base(&Some(install_location), &mountpoint),
                        Action::Mkdir(mountpoint),
                    )
                })
            })
            .collect();

        // mkdir -p /{DEFAULT_CHROOT_LOC}/{mkdir_path}
        for (dir, action_mkdir) in mountpoints {
            if let Err(err) = shell::exec("mkdir", &[&dir]) {
                return Err(AliError::InstallError {
                    error: Box::new(err),
                    action_failed: Box::new(action_mkdir),
                    actions_performed: actions,
                });
            }

            actions.push(action_mkdir);
        }

        // Mount other filesystems under /{DEFAULT_CHROOT_LOC}
        match fs::mount_filesystems(filesystems, install_location) {
            Err(err) => {
                return Err(AliError::InstallError {
                    error: Box::new(err),
                    action_failed: Box::new(Action::MountFilesystems),
                    actions_performed: actions,
                });
            }
            Ok(actions_mount_filesystems) => actions.extend(actions_mount_filesystems),
        }
    }

    // Collect packages, with base as bare-minimum
    let mut packages = HashSet::from(["base".to_string()]);
    if let Some(pacstraps) = manifest.pacstraps.clone() {
        packages.extend(pacstraps);
    }

    // Install packages (manifest.pacstraps) to install_location
    let action_pacstrap = Action::InstallPackages { packages };
    if let Err(err) = pacstrap_to_location(&manifest.pacstraps, install_location) {
        return Err(AliError::InstallError {
            error: Box::new(err),
            action_failed: Box::new(action_pacstrap),
            actions_performed: actions,
        });
    }
    actions.push(action_pacstrap);

    // Apply ALI routine installation outside of arch-chroot
    let action_ali_routine = Action::AliRoutine;
    match routine::apply_routine(manifest, install_location) {
        Err(err) => {
            return Err(AliError::InstallError {
                error: Box::new(err),
                action_failed: Box::new(action_ali_routine),
                actions_performed: actions,
            });
        }
        Ok(actions_routine) => {
            actions.extend(actions_routine);
            actions.push(action_ali_routine);
        }
    }

    // Apply ALI routine installation in arch-chroot
    let action_ali_archchroot = Action::AliArchChroot;
    match archchroot::ali(manifest, install_location) {
        Err(err) => {
            return Err(AliError::InstallError {
                error: Box::new(err),
                action_failed: Box::new(action_ali_archchroot),
                actions_performed: actions,
            });
        }
        Ok(actions_archchroot) => {
            actions.extend(actions_archchroot);
            actions.push(action_ali_archchroot);
        }
    }

    // Apply manifest.chroot
    if let Some(ref cmds) = manifest.chroot {
        let action_user_archchroot = Action::UserArchChroot;

        match archchroot::user_chroot(cmds.iter(), install_location) {
            Err(err) => {
                return Err(AliError::InstallError {
                    error: Box::new(err),
                    action_failed: Box::new(action_user_archchroot),
                    actions_performed: actions,
                });
            }
            Ok(actions_user_cmds) => {
                actions.extend(actions_user_cmds);
                actions.push(action_user_archchroot);
            }
        }
    }

    // Apply manifest.postinstall with sh -c 'cmd'
    if let Some(ref cmds) = manifest.postinstall {
        let action_user_postinstall = Action::UserPostInstall;

        for cmd in cmds {
            let action_postinstall_cmd = Action::UserPostInstallCmd(cmd.clone());
            if let Err(err) = shell::sh_c(cmd) {
                return Err(AliError::InstallError {
                    error: Box::new(err),
                    action_failed: Box::new(action_user_postinstall),
                    actions_performed: actions,
                });
            }

            actions.push(action_postinstall_cmd);
        }

        actions.push(action_user_postinstall);
    }

    Ok(actions)
}

fn pacstrap_to_location(
    pacstraps: &Option<HashSet<String>>,
    location: &str,
) -> Result<(), AliError> {
    // Collect packages, with base as bare-minimum
    let mut packages = HashSet::from(["base".to_string()]);
    if let Some(pacstraps) = pacstraps.clone() {
        packages.extend(pacstraps);
    }

    let cmd_pacstrap = {
        let mut cmd_parts = vec![
            "pacstrap".to_string(),
            "-K".to_string(),
            location.to_string(),
        ];
        cmd_parts.extend(packages);
        cmd_parts.join(" ")
    };

    shell::sh_c(&cmd_pacstrap)
}