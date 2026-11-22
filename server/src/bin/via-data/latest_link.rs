use std::{
    error::Error,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

pub(crate) fn update_latest_static_link(output: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let (output_directory, build_directory_name) = split_output_path(output, "build output")?;
    let latest_link = output_directory.join(".via-static-latest.json");
    let target = PathBuf::from(build_directory_name).join(".via-static.json");
    replace_latest_link(&latest_link, &target, create_file_symlink)?;
    Ok(latest_link)
}

pub(crate) fn update_latest_fetch_link(snapshot: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let (fetch_directory, snapshot_directory_name) = split_output_path(snapshot, "fetch snapshot")?;
    let latest_link = fetch_directory.join("latest");
    replace_latest_link(
        &latest_link,
        Path::new(snapshot_directory_name),
        create_directory_symlink,
    )?;
    Ok(latest_link)
}

fn split_output_path<'a>(
    path: &'a Path,
    description: &str,
) -> Result<(&'a Path, &'a std::ffi::OsStr), Box<dyn Error>> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            ErrorKind::InvalidInput,
            format!("{description} has no parent directory"),
        )
    })?;
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            ErrorKind::InvalidInput,
            format!("{description} has no directory name"),
        )
    })?;
    Ok((parent, name))
}

fn replace_latest_link(
    latest_link: &Path,
    target: &Path,
    create_symlink: fn(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), Box<dyn Error>> {
    match fs::symlink_metadata(latest_link) {
        Ok(metadata) if metadata.file_type().is_symlink() => remove_symlink(latest_link)?,
        Ok(_) => {
            return Err(format!(
                "refusing to replace non-symlink latest path {}",
                latest_link.display()
            )
            .into());
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    create_symlink(target, latest_link)?;
    Ok(())
}

#[cfg(unix)]
fn remove_symlink(link: &Path) -> std::io::Result<()> {
    fs::remove_file(link)
}

#[cfg(windows)]
fn remove_symlink(link: &Path) -> std::io::Result<()> {
    fs::remove_file(link).or_else(|_| fs::remove_dir(link))
}

#[cfg(unix)]
fn create_file_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_file_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

#[cfg(unix)]
fn create_directory_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_directory_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::update_latest_fetch_link;

    #[test]
    fn latest_fetch_link_follows_the_newest_snapshot_and_preserves_real_directories() {
        let root = std::env::temp_dir().join(format!(
            "via-data-latest-fetch-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let older = root.join("20260101T000000Z");
        let newer = root.join("20260201T000000Z");
        fs::create_dir_all(&older).unwrap();
        fs::create_dir_all(&newer).unwrap();
        fs::write(older.join("manifest.json"), "older").unwrap();
        fs::write(newer.join("manifest.json"), "newer").unwrap();

        let latest = update_latest_fetch_link(&older).unwrap();
        assert_eq!(latest, root.join("latest"));
        assert_eq!(
            fs::read_to_string(latest.join("manifest.json")).unwrap(),
            "older"
        );
        update_latest_fetch_link(&newer).unwrap();
        assert_eq!(
            fs::read_link(&latest).unwrap(),
            std::path::PathBuf::from("20260201T000000Z")
        );
        assert_eq!(
            fs::read_to_string(latest.join("manifest.json")).unwrap(),
            "newer"
        );

        fs::remove_file(&latest).unwrap();
        fs::create_dir(&latest).unwrap();
        assert!(update_latest_fetch_link(&newer).is_err());
        assert!(fs::symlink_metadata(&latest).unwrap().is_dir());

        fs::remove_dir_all(&root).unwrap();
    }
}
