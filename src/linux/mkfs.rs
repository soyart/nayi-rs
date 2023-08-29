use crate::errors::AliError;
use crate::manifest;
use crate::utils::shell;

/// Executes:
/// ```shell
/// mkfs.{fs.fs_type} {fs.fs_opts} {fs.device}
/// ```
pub fn create_fs(fs: manifest::ManifestFs) -> Result<(), AliError> {
    let cmd_mkfs = match fs.fs_opts {
        Some(opts) => format!("'mkfs.{} {opts} {}'", fs.fs_type, fs.device),
        None => format!("'mkfs.{} {}'", fs.fs_type, fs.device),
    };

    shell::exec("sh", &["-c", &cmd_mkfs])
}