/* SPDX-License-Identifier: MIT */

use crate::config::Device;
use anyhow::{anyhow, Context, Result};
use log::{info, log, warn, Level};
use std::cmp;
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

fn make_parent(of: &Path) -> Result<()> {
    let parent = of
        .parent()
        .ok_or_else(|| anyhow!("Couldn't get parent of {}", of.display()))?;
    fs::create_dir_all(&parent)?;
    Ok(())
}

fn make_symlink(dst: &str, src: &Path) -> Result<()> {
    make_parent(src)?;
    symlink(dst, src)
        .with_context(|| format!("Failed to create symlink {}→{}", src.display(), dst))?;
    Ok(())
}

fn virtualization_container() -> Result<bool> {
    let mut child = match Command::new("systemd-detect-virt")
        .arg("--quiet")
        .arg("--container")
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            warn!(
                "systemd-detect-virt call failed, assuming we're not in a container: {}",
                e
            );
            return Ok(false);
        }
    };

    match child.wait() {
        Ok(status) => Ok(status.success()),
        Err(e) => Err(anyhow!("systemd-detect-virt call failed: {}", e)),
    }
}

fn modprobe(modname: &str, required: bool) {
    let status = Command::new("modprobe").arg(modname).status();
    match status {
        Err(e) => {
            let level = match !required && e.kind() == io::ErrorKind::NotFound {
                true => Level::Debug,
                false => Level::Warn,
            };

            log!(
                level,
                "modprobe \"{}\" cannot be spawned, ignoring: {}",
                modname,
                e
            );
        }
        Ok(status) => {
            if !status.success() {
                warn!("modprobe \"{}\" failed, ignoring: code {}", modname, status);
            }
        }
    };
}

pub fn run_generator(devices: &[Device], output_directory: &Path, fake_mode: bool) -> Result<()> {
    if devices.is_empty() {
        info!("No devices configured, exiting.");
        return Ok(());
    }

    if virtualization_container()? && !fake_mode {
        info!("Running in a container, exiting.");
        return Ok(());
    }

    for device in devices {
        handle_device(output_directory, device)?;
    }

    if !devices.is_empty() && !fake_mode {
        /* We created some units, let's make sure the module is loaded and the devices exist */
        if !Path::new("/sys/class/zram-control").exists() {
            modprobe("zram", true);
        }

        let max_device = devices
            .iter()
            .map(|device| {
                device.name[4..]
                    .parse()
                    .expect("already verified in read_devices()")
            })
            .fold(0, cmp::max);

        if !Path::new("/dev")
            .join(format!("zram{}", max_device))
            .exists()
        {
            while fs::read_to_string("/sys/class/zram-control/hot_add")
                .context("Adding zram device")?
                .trim_end()
                .parse::<u64>()
                .context("Fresh zram device number")?
                < max_device
            {}
        }
    }

    let compressors: BTreeSet<_> = devices
        .iter()
        .flat_map(|device| device.compression_algorithm.as_deref())
        .collect();

    if !compressors.is_empty() {
        let proc_crypto = match fs::read_to_string("/proc/crypto") {
            Ok(string) => string,
            Err(e) => {
                warn!("Failed to read /proc/crypto, proceeding as if empty: {}", e);
                String::from("")
            }
        };
        let known = parse_known_compressors(&proc_crypto);

        for comp in compressors.difference(&known) {
            modprobe(&format!("crypto-{}", comp), false);
        }
    }

    Ok(())
}

// Returns a list of names of loaded compressors
fn parse_known_compressors(proc_crypto: &str) -> BTreeSet<&str> {
    // Extract algorithm names (this includes non-compression algorithms too)
    proc_crypto
        .lines()
        .into_iter()
        .filter(|line| line.starts_with("name"))
        .map(|m| m.rsplit(':').next().unwrap().trim())
        .collect()
}

fn write_contents(output_directory: &Path, filename: &str, contents: &str) -> Result<()> {
    let path = output_directory.join(filename);
    make_parent(&path)?;

    let contents = format!(
        "\
# Automatically generated by {exe_name}

{contents}",
        exe_name = std::env::current_exe().unwrap().display(),
        contents = contents
    );

    fs::write(&path, contents).with_context(|| format!("Failed to write {:?}", path))
}

fn handle_device(output_directory: &Path, device: &Device) -> Result<()> {
    if device.is_swap() {
        handle_zram_swap(output_directory, device)
    } else {
        handle_zram_mount_point(output_directory, device)
    }
}

fn handle_zram_swap(output_directory: &Path, device: &Device) -> Result<()> {
    let swap_name = format!("dev-{}.swap", device.name);

    info!(
        "Creating unit {} (/dev/{} with {}MB)",
        swap_name,
        device.name,
        device.disksize / 1024 / 1024
    );

    /* systemd-zram-setup@.service.
     * We use the packaged unit, and only need to provide a small drop-in. */

    write_contents(
        output_directory,
        &format!(
            "systemd-zram-setup@{}.service.d/bindsto-swap.conf",
            device.name
        ),
        "\
[Unit]
BindsTo=dev-%i.swap
",
    )?;

    /* dev-zramX.swap */

    write_contents(
        output_directory,
        &swap_name,
        &format!(
            "\
[Unit]
Description=Compressed Swap on /dev/{zram_device}
Documentation=man:zram-generator(8) man:zram-generator.conf(5)
Requires=systemd-zram-setup@{zram_device}.service
After=systemd-zram-setup@{zram_device}.service

[Swap]
What=/dev/{zram_device}
Priority={swap_priority}
",
            zram_device = device.name,
            swap_priority = device.swap_priority
        ),
    )?;

    /* enablement symlink */

    let symlink_path = output_directory.join("swap.target.wants").join(&swap_name);
    let target_path = format!("../{}", swap_name);
    make_symlink(&target_path, &symlink_path)?;

    Ok(())
}

fn mount_unit_name(path: &Path) -> String {
    /* FIXME: handle full escaping */
    assert!(path.is_absolute());

    let path = path.strip_prefix("/").unwrap().to_str().unwrap();
    format!("{}.mount", path.replace("/", "-"))
}

fn handle_zram_mount_point(output_directory: &Path, device: &Device) -> Result<()> {
    if device.mount_point.is_none() {
        /* In this case we don't need to generate any units. */
        return Ok(());
    }

    let mount_name = &mount_unit_name(device.mount_point.as_ref().unwrap());

    info!(
        "Creating unit {} (/dev/{} with {}MB)",
        mount_name,
        device.name,
        device.disksize / 1024 / 1024
    );

    /* systemd-zram-setup@.service.
     * We use the packaged unit, and only need to provide a small drop-in. */

    write_contents(
        output_directory,
        &format!(
            "systemd-zram-setup@{}.service.d/bindsto-mount.conf",
            device.name
        ),
        &format!(
            "\
[Unit]
BindsTo={}
",
            mount_name
        ),
    )?;

    write_contents(
        output_directory,
        &mount_name,
        &format!(
            "\
[Unit]
Description=Compressed Storage on /dev/{zram_device}
Documentation=man:zram-generator(8) man:zram-generator.conf(5)
Requires=systemd-zram-setup@{zram_device}.service
After=systemd-zram-setup@{zram_device}.service

[Mount]
What=/dev/{zram_device}
Where={mount_point:?}
",
            zram_device = device.name,
            mount_point = device.mount_point.as_ref().unwrap(),
        ),
    )?;

    /* enablement symlink */

    let symlink_path = output_directory
        .join("local-fs.target.wants")
        .join(&mount_name);
    let target_path = format!("../{}", mount_name);
    make_symlink(&target_path, &symlink_path)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::iter::FromIterator;

    #[test]
    fn test_parse_known_compressors() {
        let data = "\
name         : zstd
driver       : zstd-scomp
module       : zstd
priority     : 0
refcnt       : 1
selftest     : passed
internal     : no
type         : scomp

name         : zstd
driver       : zstd-generic
module       : zstd
priority     : 0
refcnt       : 1
selftest     : passed
internal     : no
type         : compression

name         : ccm(aes)
driver       : ccm_base(ctr(aes-aesni),cbcmac(aes-aesni))
module       : ccm
priority     : 300
refcnt       : 2
selftest     : passed
internal     : no
type         : aead
async        : no
geniv        : <none>

name         : ctr(aes)
driver       : ctr(aes-aesni)
module       : kernel
priority     : 300
refcnt       : 2
selftest     : passed
internal     : no
type         : skcipher
";
        let expected = vec!["zstd", "ccm(aes)", "ctr(aes)"];
        assert_eq!(parse_known_compressors(data), BTreeSet::from_iter(expected));
    }
}
