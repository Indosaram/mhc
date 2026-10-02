#[test]
fn malformed_json_diagnostics_equal_bun(){
    let inputs=["","{invalid","{foo:1}","{true:1}","{1:2}","{\"a\":1,}","{\"a\":1,,\"b\":2}","{\"a\" 1}","{\"a\":}","[1,]","[1,,2]","true false","tru","nullx","1.","1e","01","-","\"a\n\"","\"\\q\"","{\"a\":1 \"b\":2}","\"\\u12\"","\"\\uXXXX\"","[1,","{ \"a\":","{\"a\":1,","@","NaN","Infinity","{\"a\": undefined}","{\"a\": /*x*/1}","한","\"\\한\"","😀"];
    let output=std::process::Command::new("bun").args(["-e","const inputs=JSON.parse(process.argv[1]);console.log(JSON.stringify(inputs.map(input=>{try{JSON.parse(input);return null}catch(error){return Array.from({length:error.message.length},(_,i)=>error.message.charCodeAt(i))}})))",&serde_json::to_string(inputs.as_slice()).unwrap()]).output().unwrap();
    assert!(output.status.success());
    let expected:Vec<Vec<u16>>=serde_json::from_slice(&output.stdout).unwrap();
    for(input,expected)in inputs.iter().zip(expected){assert!(serde_json::from_str::<serde_json::Value>(input).is_err());assert_eq!(maho_rpc::connection_handler::json_parse_error_code_units(input),expected,"input {input:?}");}
}

#[test]
fn malformed_wire_preserves_javascript_lone_surrogate_error(){
    let line=maho_rpc::connection_handler::json_parse_error_response("😀");
    let output=std::process::Command::new("bun").args(["-e","const actual=JSON.parse(process.argv[1]);let expected;try{JSON.parse(process.argv[2])}catch(error){expected='Failed to parse command: '+error.message}if(actual.type!=='response'||actual.command!=='parse'||actual.success!==false||actual.error!==expected)process.exit(1)",&line,"😀"]).output().unwrap();
    assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
}
