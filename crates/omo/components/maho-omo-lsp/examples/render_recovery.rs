use maho_ext_api::{Theme, ToolRenderContext, ToolRenderResultOptions};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let width = std::env::args().nth(1).unwrap_or_else(|| "80".into()).parse::<usize>()?;
    let mut theme = Theme::default();
    for (key, code) in [("toolTitle", "36"), ("accent", "34"), ("muted", "90"), ("dim", "90"), ("success", "32"), ("warning", "33"), ("error", "31"), ("toolOutput", "39")] {
        theme.colors.insert(key.into(), format!("\x1b[{code}m"));
    }
    let args = json!({"filePath":"/tmp/project/src/main.rs","line":4,"character":2,"scope":"document","newName":"renamed"});
    let location = json!({"uri":"file:///tmp/project/src/main.rs","range":{"start":{"line":3,"character":2}}});
    let cases = [
        ("lsp_diagnostics", json!({"totalDiagnostics":1,"diagnostics":[{"file":"/tmp/project/src/main.rs","diagnostic":{"severity":1,"range":{"start":{"line":3,"character":2}},"message":"Mismatched value: 한국어 diagnostic"}}]})),
        ("lsp_goto_definition", json!({"locations":[location.clone()]})),
        ("lsp_find_references", json!({"totalReferences":1,"references":[location]})),
        ("lsp_symbols", json!({"totalSymbols":1,"scope":"document","symbols":[{"name":"Root","kind":12,"range":{"start":{"line":0}},"children":[{"name":"Child","kind":12,"range":{"start":{"line":2}}}]}]})),
        ("lsp_prepare_rename", json!({})),
        ("lsp_rename", json!({"apply":{"success":true,"totalEdits":2,"filesModified":["/tmp/project/src/main.rs"],"errors":[]}})),
    ];
    for (name, details) in cases {
        let renderers = maho_omo_lsp::adapter::renderers::tool_renderers(name).ok_or("renderer missing")?;
        let mut context = ToolRenderContext {args:args.clone(),tool_call_id:name.into(),invalidate:std::rc::Rc::new(||{}),last_component:None,state:(),cwd:"/tmp/project".into(),execution_started:true,args_complete:true,is_partial:false,expanded:true,show_images:false,image_protocol:None,is_error:false,has_result:Some(true),spinner_frame:None};
        let mut call = renderers.render_call.ok_or("call missing")?(&args,&theme,&mut context);
        for line in call.render(width) {println!("{line}\r");}
        let result = maho_ext_api::AgentToolResult {content:serde_json::from_value(json!([{"type":"text","text":"Rename ready"}]))?,details,structured_content: None, usage:None,nested_calls:None,added_tool_names:None,terminate:None,is_error:None};
        let mut output = renderers.render_result.ok_or("result missing")?(&result,ToolRenderResultOptions {expanded:true,is_partial:false},&theme,&mut context);
        for line in output.render(width) {println!("{line}\r");}
        println!("\r");
    }
    Ok(())
}
