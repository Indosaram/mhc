use maho_ext_pi_rules::rules::scanner::{scan_rule_files,ScanOptions};

fn main()->Result<(),Box<dyn std::error::Error>>{
    let root=tempfile::tempdir()?;
    for name in ["z.md","ä.md","å.md","a.md","ö.md"]{std::fs::write(root.path().join(name),"rule")?;}
    let names=scan_rule_files(ScanOptions{root_dir:root.path(),excluded_dirs:None,max_depth:None}).into_iter().map(|file|file.path.file_name().ok_or("name").map(|name|name.to_string_lossy().into_owned())).collect::<Result<Vec<_>,_>>()?;
    println!("{}",serde_json::to_string(&names)?);
    Ok(())
}
