#[derive(Clone,Debug,PartialEq,Eq)]
pub struct PatchDiff { pub diff:String,pub added:usize,pub removed:usize }
pub fn create_patch_diff(old:&str,new:&str)->PatchDiff {
    let width=old.split('\n').count().max(new.split('\n').count()).to_string().len();
    let (mut old_line,mut new_line,mut added,mut removed)=(1,1,0,0); let mut output=Vec::new();
    if old==new {
        for line in old.split_inclusive('\n') { output.push(format!(" {old_line:>width$} {}",line.strip_suffix('\n').unwrap_or(line))); old_line+=1; }
    } else {
        let context=old.split('\n').count()+new.split('\n').count();
        let patch=maho_tools::unified_diff::create_unified_patch("old","new",old,new,context);
        for line in patch.split('\n').skip(2) {
            if line.starts_with("@@") || line.starts_with("\\ No newline") { continue; }
            let Some(kind)=line.chars().next() else { continue; }; let value=&line[1..];
            match kind {
                '+'=>{ output.push(format!("+{new_line:>width$} {value}")); new_line+=1; added+=1; },
                '-'=>{ output.push(format!("-{old_line:>width$} {value}")); old_line+=1; removed+=1; },
                ' '=>{ output.push(format!(" {old_line:>width$} {value}")); old_line+=1; new_line+=1; },_=>{},
            }
        }
    }
    PatchDiff{diff:output.join("\n"),added,removed}
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn numbered_diff_preserves_carriage_returns() { let result=create_patch_diff("old\r\n","new\r\n"); assert_eq!(result.diff,"-1 old\r\n+1 new\r"); assert_eq!((result.added,result.removed),(1,1)); }
    #[test] fn numbered_replacement_preserves_context() { assert_eq!(create_patch_diff("same\nold\n","same\nnew\n"),PatchDiff{diff:" 1 same\n-2 old\n+2 new".into(),added:1,removed:1}); }
    #[test] fn identical_content_is_not_elided() { assert_eq!(create_patch_diff("a\nb","a\nb"),PatchDiff{diff:" 1 a\n 2 b".into(),added:0,removed:0}); assert!(create_patch_diff("","").diff.is_empty()); }
    #[test] fn newline_only_change_counts_both_lines() { assert_eq!(create_patch_diff("a","a\n").diff,"-1 a\n+1 a"); }
}
