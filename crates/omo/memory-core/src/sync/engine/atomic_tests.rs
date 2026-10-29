use super::*;

#[test]
fn write_atomic_creates_parents_and_leaves_no_temp_file() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempfile::tempdir()?;
    let target = dir.path().join("a/b/file.txt");
    write_atomic(&target, b"hello")?;
    assert_eq!(std::fs::read_to_string(&target)?, "hello");
    let names: Vec<_> = std::fs::read_dir(target.parent().ok_or("no parent")?)?
        .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_, _>>()?;
    assert_eq!(names, vec!["file.txt".to_string()]);
    Ok(())
}

#[test]
fn read_json_falls_back_on_missing_and_write_json_ends_with_newline()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("x.json");
    assert_eq!(read_json(&path, vec![1u32])?, vec![1u32]);
    write_json(&path, &vec![2u32, 3])?;
    assert_eq!(std::fs::read_to_string(&path)?, "[2,3]\n");
    assert_eq!(read_json(&path, Vec::<u32>::new())?, vec![2, 3]);
    Ok(())
}

#[test]
fn remove_atomic_ignores_missing_file() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    remove_atomic(&dir.path().join("gone"))?;
    Ok(())
}
