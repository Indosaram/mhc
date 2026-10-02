use maho_ext_pi_webfetch::webfetch::{errors::WebfetchError,fetcher::*};
#[tokio::main]async fn main()->Result<(),Box<dyn std::error::Error>>{
    let listener=std::net::TcpListener::bind("127.0.0.1:0")?;let address=listener.local_addr()?;drop(listener);
    let result=fetch_url(&format!("http://{address}"),WebfetchFormat::Text,Some(1.0)).await;
    match result{Err(WebfetchError::Network(error))if error.is_connect()=>println!("unreachable=connection refused"),_=>return Err("unreachable-host QA mismatch".into())}
    let result=fetch_url("file:///fixture",WebfetchFormat::Text,None).await;
    if !matches!(result,Err(WebfetchError::InvalidUrl(_))){return Err("URL boundary QA mismatch".into());}
    println!("invalidScheme=rejected cleanup=no server or temporary files");Ok(())
}
